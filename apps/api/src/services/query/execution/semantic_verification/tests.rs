//! Neutral protocol fixtures: mock judgments prove validation, not semantic truth.

use ironrag_contracts::assistant::semantic_verification::{
    SemanticClaimSupport, SemanticCompleteness, SemanticEvidenceScope, SemanticFailureCode,
    SemanticNotRunReason, SemanticResponseClassification, SemanticSourceLocator,
    SemanticVerificationOutcome,
};
use serde_json::{Value, json};
use uuid::Uuid;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn completed_response_cannot_be_replayed_across_questions() -> TestResult {
    let evidence = source("A neutral source with a stated prerequisite.");
    let first =
        prepare("What is the prerequisite?", "A candidate answer.", vec![evidence.clone()])?;
    let second = prepare("What essential caveat applies?", "A candidate answer.", vec![evidence])?;
    assert_eq!(first.answer_sha256(), second.answer_sha256());
    assert_eq!(first.manifest_sha256(), second.manifest_sha256());
    assert_ne!(first.question_sha256(), second.question_sha256());
    assert_eq!(first.request_json()["questionSha256"], first.question_sha256());
    let completed = response(&first);
    assert_eq!(parse(&first, &completed)?.question_sha256, first.question_sha256());
    assert_eq!(parse(&second, &completed).err(), Some(SemanticValidationError::IdentityMismatch));
    Ok(())
}

fn source(text: &str) -> AuthorizedSemanticSource {
    AuthorizedSemanticSource {
        document_id: Uuid::from_u128(1),
        document_revision_id: Uuid::from_u128(2),
        locator: SemanticSourceLocator::DocumentSlice { start: 0, end: text.len() },
        text: text.into(),
        complete_document: None,
    }
}

fn complete_source(text: &str) -> AuthorizedSemanticSource {
    AuthorizedSemanticSource {
        complete_document: Some(CompleteDocumentProof {
            canonical_text_sha256: preparation::sha256(text.as_bytes()),
            canonical_utf8_bytes: text.len(),
        }),
        ..source(text)
    }
}

fn prepared(answer: &str) -> Result<PreparedSemanticVerification, SemanticPreparationError> {
    prepare(
        "What does the selected source establish?",
        answer,
        vec![source("The interval is ten seconds. Keep the guard enabled. café.")],
    )
}

fn quote(input: &PreparedSemanticVerification) -> Value {
    let source = &input.sources()[0];
    json!({"sourceId":source.source_id,"quote":source.text})
}

fn response(input: &PreparedSemanticVerification) -> Value {
    let units: Vec<_> = input.units().iter().map(|unit| json!({
        "unitId":unit.unit_id,"support":"supported","assertsDocumentAbsence":false,"evidence":[quote(input)]
    })).collect();
    json!({
        "protocolVersion":1,"questionSha256":input.question_sha256(),"checkedAnswerSha256":input.answer_sha256(),"evidenceManifestSha256":input.manifest_sha256(),
        "units":units,"completeness":"complete","missingCaveats":[],"responseClassification":"factual_answer"
    })
}

fn parse(
    input: &PreparedSemanticVerification,
    value: &Value,
) -> Result<
    ironrag_contracts::assistant::semantic_verification::SemanticVerificationReport,
    SemanticValidationError,
> {
    input.parse_response(
        StructuredSemanticOutput {
            json: &value.to_string(),
            status: SemanticOutputStatus::Complete,
            output_tokens: Some(200),
        },
        11,
        Some(Uuid::from_u128(9)),
    )
}

#[test]
fn preparation_units_exhaust_exact_utf8_answer_and_stable_ids() -> TestResult {
    let answer = "\n\nFirst assertion.\n\n  \nDeuxième assertion.\n\n";
    let input = prepared(answer)?;
    assert_eq!(input.units().len(), 2);
    assert_eq!(input.units().iter().map(|unit| unit.text.as_str()).collect::<String>(), answer);
    let mut end = 0;
    for unit in input.units() {
        assert_eq!(unit.span.start, end);
        assert_eq!(answer.get(unit.span.start..unit.span.end), Some(unit.text.as_str()));
        assert!(!unit.text.trim().is_empty());
        end = unit.span.end;
    }
    assert_eq!(end, answer.len());
    assert_eq!(input.units(), prepared(answer)?.units());
    assert_ne!(input.answer_sha256(), prepared(&format!("{answer} "))?.answer_sha256());
    Ok(())
}

#[test]
fn preparation_hashes_bind_exact_source_identity_scope_text_and_locator() -> TestResult {
    let text = "A neutral document.";
    let base = source(text);
    let a = prepare("Q", "A", vec![base.clone()])?;
    for other in [
        AuthorizedSemanticSource { document_id: Uuid::from_u128(3), ..base.clone() },
        AuthorizedSemanticSource { document_revision_id: Uuid::from_u128(3), ..base.clone() },
        AuthorizedSemanticSource {
            locator: SemanticSourceLocator::Segment { segment_id: Uuid::from_u128(3) },
            ..base.clone()
        },
        source("A neutral document!"),
        complete_source(text),
    ] {
        assert_ne!(a.manifest_sha256(), prepare("Q", "A", vec![other])?.manifest_sha256());
    }
    let other = source("Different source.");
    assert_eq!(
        prepare("Q", "A", vec![base.clone(), other.clone()])?.manifest_sha256(),
        prepare("Q", "A", vec![other, base.clone()])?.manifest_sha256()
    );
    assert_eq!(
        prepare("Q", "A", vec![base.clone(), base]).err(),
        Some(SemanticPreparationError::DuplicateSource)
    );
    Ok(())
}

#[test]
fn provider_payload_exposes_only_opaque_source_references() -> TestResult {
    let input = prepared("The interval is ten seconds.")?;
    let request = input.request_json();
    let supplied = &request["sources"][0];
    assert!(supplied.get("locator").is_none());
    assert!(supplied.get("documentId").is_none());
    assert!(supplied.get("documentRevisionId").is_none());
    assert_eq!(supplied["sourceId"], input.sources()[0].source_id);
    assert_eq!(supplied["scope"], "selected_excerpt");
    assert!(!SYSTEM_PROMPT.is_empty());
    assert!(response_json_schema().is_object());
    Ok(())
}

#[test]
fn complete_document_proof_cannot_promote_excerpt_or_wrong_revision_text() -> TestResult {
    let mut item = complete_source("Full canonical text.");
    let input = prepare("Q", "A", vec![item.clone()])?;
    assert_eq!(input.sources()[0].scope, SemanticEvidenceScope::CompleteDocument);
    item.text = "Full".into();
    item.locator = SemanticSourceLocator::DocumentSlice { start: 0, end: 4 };
    assert_eq!(
        prepare("Q", "A", vec![item]).err(),
        Some(SemanticPreparationError::InvalidCompleteDocumentProof)
    );
    let mut item = complete_source("Canonical");
    item.locator = SemanticSourceLocator::Chunk { chunk_id: Uuid::from_u128(4) };
    assert_eq!(
        prepare("Q", "A", vec![item]).err(),
        Some(SemanticPreparationError::InvalidCompleteDocumentProof)
    );
    let mut item = complete_source("Canonical");
    if let Some(proof) = &mut item.complete_document {
        proof.canonical_utf8_bytes += 1;
    }
    assert_eq!(
        prepare("Q", "A", vec![item]).err(),
        Some(SemanticPreparationError::InvalidCompleteDocumentProof)
    );
    Ok(())
}

#[test]
fn preparation_refuses_independent_size_limits_without_truncation() -> TestResult {
    assert_eq!(
        prepared(&"x".repeat(MAX_ANSWER_UTF8_BYTES + 1)).err(),
        Some(SemanticPreparationError::AnswerTooLarge)
    );
    assert_eq!(
        prepared(&"x\n".repeat(MAX_ANSWER_UNITS + 1)).err(),
        Some(SemanticPreparationError::TooManyUnits)
    );
    assert_eq!(
        prepare("Q", "A", vec![source(&"x".repeat(MAX_EVIDENCE_UTF8_BYTES + 1))]).err(),
        Some(SemanticPreparationError::EvidenceTooLarge)
    );
    let sources: Vec<_> =
        (0..=MAX_SOURCE_ENTRIES).map(|i| source(&format!("source {i}"))).collect();
    assert_eq!(prepare("Q", "A", sources).err(), Some(SemanticPreparationError::TooManySources));
    assert_eq!(
        prepare(&"q".repeat(MAX_QUESTION_UTF8_BYTES + 1), "A", vec![source("S")]).err(),
        Some(SemanticPreparationError::QuestionTooLarge)
    );
    assert_eq!(
        prepare(
            "Q",
            "A",
            vec![
                source(&"x".repeat(MAX_EVIDENCE_UTF8_BYTES / 2 + 1)),
                source(&"y".repeat(MAX_EVIDENCE_UTF8_BYTES / 2 + 1))
            ]
        )
        .err(),
        Some(SemanticPreparationError::EvidenceTooLarge)
    );
    assert_eq!(prepared(&"x".repeat(MAX_ANSWER_UTF8_BYTES))?.units().len(), 1);
    assert_eq!(
        prepared(&"é".repeat(MAX_ANSWER_UTF8_BYTES / 2 + 1)).err(),
        Some(SemanticPreparationError::AnswerTooLarge)
    );
    assert_eq!(prepared(&"x\n".repeat(MAX_ANSWER_UNITS))?.units().len(), MAX_ANSWER_UNITS);
    assert!(prepare("Q", "A", vec![source(&"x".repeat(MAX_EVIDENCE_UTF8_BYTES))]).is_ok());
    let sources = (0..MAX_SOURCE_ENTRIES).map(|i| source(&format!("entry {i}"))).collect();
    assert_eq!(prepare("Q", "A", sources)?.sources().len(), MAX_SOURCE_ENTRIES);
    Ok(())
}

#[test]
fn preparation_rejects_empty_and_invalid_sources() {
    assert_eq!(prepared(" \n").err(), Some(SemanticPreparationError::EmptyAnswer));
    assert_eq!(prepare("Q", "A", vec![]).err(), Some(SemanticPreparationError::NoEvidence));
    assert_eq!(
        prepare("Q", "A", vec![source(" ")]).err(),
        Some(SemanticPreparationError::InvalidSource)
    );
    let mut item = source("abc");
    item.locator = SemanticSourceLocator::DocumentSlice { start: 4, end: 2 };
    assert_eq!(prepare("Q", "A", vec![item]).err(), Some(SemanticPreparationError::InvalidSource));
    let mut item = source("abc");
    item.document_revision_id = Uuid::nil();
    assert_eq!(prepare("Q", "A", vec![item]).err(), Some(SemanticPreparationError::InvalidSource));
}

#[test]
fn supported_report_is_bound_ordered_and_formal_output_neutral() -> TestResult {
    let input = prepared("First assertion.\nSecond assertion.")?;
    let mut value = response(&input);
    value["units"].as_array_mut().ok_or("missing units")?.reverse();
    let report = parse(&input, &value)?;
    assert_eq!(report.checked_answer_sha256, input.answer_sha256());
    assert_eq!(report.visible_answer_sha256, None);
    assert_eq!(report.answer_units, input.units());
    assert_eq!(report.sources, input.sources());
    assert_eq!(report.evidence_manifest_sha256, input.manifest_sha256());
    assert_eq!(report.elapsed_ms, 11);
    assert_eq!(report.provider_call_id, Some(Uuid::from_u128(9)));
    let SemanticVerificationOutcome::Completed { assessment } = report.outcome else {
        return Err("expected completed".into());
    };
    assert_eq!(assessment.claim_support, SemanticClaimSupport::Supported);
    assert_eq!(assessment.units[0].unit_id, input.units()[0].unit_id);
    // A provider cannot smuggle a formal/readiness upgrade into the protocol.
    value["verificationState"] = json!("verified");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidJson));
    let mut forged_visible = response(&input);
    forged_visible["visibleAnswerSha256"] = json!("a".repeat(64));
    assert_eq!(parse(&input, &forged_visible).err(), Some(SemanticValidationError::InvalidJson));
    Ok(())
}

#[test]
fn mixed_and_contradicted_unit_support_are_not_aggregated_as_supported() -> TestResult {
    let input = prepared("First assertion.\nSecond assertion.")?;
    let mut value = response(&input);
    value["units"][1]["support"] = json!("insufficient_evidence");
    value["units"][1]["evidence"] = json!([]);
    let SemanticVerificationOutcome::Completed { assessment } = parse(&input, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_eq!(assessment.claim_support, SemanticClaimSupport::Mixed);
    value["units"][1]["support"] = json!("contradicted");
    value["units"][1]["evidence"] = json!([quote(&input)]);
    let SemanticVerificationOutcome::Completed { assessment } = parse(&input, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_eq!(assessment.claim_support, SemanticClaimSupport::Contradicted);
    let single = prepared("A block with multiple assertions.")?;
    let mut value = response(&single);
    value["units"][0]["support"] = json!("mixed");
    let SemanticVerificationOutcome::Completed { assessment } = parse(&single, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_eq!(assessment.claim_support, SemanticClaimSupport::Mixed);
    Ok(())
}

#[test]
fn missing_essential_caveat_is_independent_of_supported_claims() -> TestResult {
    let input = prepared("The interval is ten seconds.")?;
    let mut value = response(&input);
    value["completeness"] = json!("missing_essential_caveats");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::IncoherentAssessment));
    value["missingCaveats"] =
        json!([{"description":"The guard condition was omitted.","evidence":[quote(&input)]}]);
    let SemanticVerificationOutcome::Completed { assessment } = parse(&input, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_eq!(assessment.claim_support, SemanticClaimSupport::Supported);
    assert_eq!(assessment.completeness, SemanticCompleteness::MissingEssentialCaveats);
    value["completeness"] = json!("complete");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::IncoherentAssessment));
    Ok(())
}

#[test]
fn correct_bounded_abstention_needs_complete_scope_and_does_not_verify_missing_fact() -> TestResult
{
    let excerpt = prepare(
        "Q",
        "The source does not specify the requested fact.",
        vec![source("A complete neutral text.")],
    )?;
    let full = prepare(
        "Q",
        "The source does not specify the requested fact.",
        vec![complete_source("A complete neutral text.")],
    )?;
    for (input, expected) in
        [(&excerpt, Some(SemanticValidationError::IncompleteDocumentScope)), (&full, None)]
    {
        let mut value = response(input);
        value["units"][0]["assertsDocumentAbsence"] = json!(true);
        value["responseClassification"] = json!("correct_source_bounded_abstention");
        let result = parse(input, &value);
        assert_eq!(result.as_ref().err().copied(), expected);
        if let Ok(report) = result {
            let SemanticVerificationOutcome::Completed { assessment } = report.outcome else {
                return Err("expected completed".into());
            };
            assert_eq!(
                assessment.response_classification,
                SemanticResponseClassification::CorrectSourceBoundedAbstention
            );
        }
    }
    let mut value = response(&full);
    value["responseClassification"] = json!("correct_source_bounded_abstention");
    assert_eq!(parse(&full, &value).err(), Some(SemanticValidationError::IncoherentAssessment));
    Ok(())
}

#[test]
fn claimless_false_refusal_is_not_vacuous_factual_success() -> TestResult {
    let input = prepared("I cannot provide a supported answer.")?;
    let mut value = response(&input);
    value["units"][0]["support"] = json!("no_factual_claims");
    value["units"][0]["evidence"] = json!([]);
    value["completeness"] = json!("undetermined");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::IncoherentAssessment));
    value["responseClassification"] = json!("false_refusal");
    let SemanticVerificationOutcome::Completed { assessment } = parse(&input, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_eq!(assessment.claim_support, SemanticClaimSupport::NoFactualClaims);
    assert_eq!(assessment.response_classification, SemanticResponseClassification::FalseRefusal);
    value["responseClassification"] = json!("correct_source_bounded_abstention");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::IncoherentAssessment));
    value["responseClassification"] = json!("false_refusal");
    value["completeness"] = json!("complete");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::IncoherentAssessment));
    Ok(())
}

#[test]
fn false_source_absence_can_be_contradicted_by_an_excerpt() -> TestResult {
    let input = prepared("The source does not specify an interval.")?;
    let mut value = response(&input);
    value["units"][0]["support"] = json!("contradicted");
    value["units"][0]["assertsDocumentAbsence"] = json!(true);
    value["responseClassification"] = json!("false_refusal");
    value["completeness"] = json!("undetermined");
    let SemanticVerificationOutcome::Completed { assessment } = parse(&input, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_eq!(assessment.claim_support, SemanticClaimSupport::Contradicted);
    assert_eq!(assessment.response_classification, SemanticResponseClassification::FalseRefusal);
    Ok(())
}

#[test]
fn unknown_source_forged_quote_and_provider_spans_fail_closed() -> TestResult {
    let input = prepared("A factual block.")?;
    let mut value = response(&input);
    value["units"][0]["evidence"][0]["sourceId"] = json!("s_forged");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::UnknownSource));
    let mut value = response(&input);
    value["units"][0]["evidence"][0]["quote"] = json!("invented text");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidQuote));
    let text = &input.sources()[0].text;
    let accented = text.find('é').ok_or("missing accent")?;
    for (start, end) in
        [(0, text.len()), (accented + 1, accented + 2), (0, text.len() + 1), (1, 0), (0, 0)]
    {
        let mut value = response(&input);
        value["units"][0]["evidence"][0]["span"] = json!({"start":start,"end":end});
        assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidJson));
    }
    let mut value = response(&input);
    value["units"][0]["evidence"][0]["quote"] = json!("é");
    let SemanticVerificationOutcome::Completed { assessment } = parse(&input, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_eq!(assessment.units[0].evidence[0].span.start, accented);
    assert_eq!(assessment.units[0].evidence[0].span.end, accented + "é".len());
    value["units"][0]["evidence"][0]["quote"] = json!("");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidQuote));
    Ok(())
}

#[test]
fn missing_duplicate_unknown_and_empty_unit_coverage_fail_closed() -> TestResult {
    let input = prepared("First.\nSecond.")?;
    let base = response(&input);
    let mut value = base.clone();
    value["units"] = json!([]);
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidUnitCoverage));
    let mut value = base.clone();
    value["units"] = json!([base["units"][0]]);
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidUnitCoverage));
    let mut value = base.clone();
    value["units"][1] = base["units"][0].clone();
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidUnitCoverage));
    let mut value = base;
    value["units"][1]["unitId"] = json!("u_unknown");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidUnitCoverage));
    Ok(())
}

#[test]
fn malformed_truncated_duplicate_field_and_wrapped_json_fail_closed() -> TestResult {
    let input = prepared("A fact.")?;
    for raw in [
        "{",
        "null",
        "[]",
        "```json\n{}\n```",
        "{} trailing",
        "{\"protocolVersion\":1,\"protocolVersion\":1}",
    ] {
        assert_eq!(
            input
                .parse_response(
                    StructuredSemanticOutput {
                        json: raw,
                        status: SemanticOutputStatus::Complete,
                        output_tokens: None
                    },
                    1,
                    None
                )
                .err(),
            Some(SemanticValidationError::InvalidJson)
        );
    }
    let value = response(&input).to_string();
    let duplicate_field = value.replacen('{', "{\"protocolVersion\":1,", 1);
    assert_eq!(
        input
            .parse_response(
                StructuredSemanticOutput {
                    json: &duplicate_field,
                    status: SemanticOutputStatus::Complete,
                    output_tokens: None
                },
                1,
                None
            )
            .err(),
        Some(SemanticValidationError::InvalidJson)
    );
    assert_eq!(
        input
            .parse_response(
                StructuredSemanticOutput {
                    json: &value,
                    status: SemanticOutputStatus::Truncated,
                    output_tokens: None
                },
                1,
                None
            )
            .err(),
        Some(SemanticValidationError::Truncated)
    );
    assert_eq!(
        input
            .parse_response(
                StructuredSemanticOutput {
                    json: &value[..value.len() - 1],
                    status: SemanticOutputStatus::Complete,
                    output_tokens: None
                },
                1,
                None
            )
            .err(),
        Some(SemanticValidationError::InvalidJson)
    );
    Ok(())
}

#[test]
fn stale_answer_manifest_protocol_and_forged_authority_fail_closed() -> TestResult {
    let input = prepared("A fact.")?;
    for (field, replacement) in [
        ("questionSha256", json!("c".repeat(64))),
        ("checkedAnswerSha256", json!("a".repeat(64))),
        ("evidenceManifestSha256", json!("b".repeat(64))),
        ("protocolVersion", json!(2)),
    ] {
        let mut value = response(&input);
        value[field] = replacement;
        assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::IdentityMismatch));
    }
    assert_eq!(
        parse(&prepared("Changed answer.")?, &response(&input)).err(),
        Some(SemanticValidationError::IdentityMismatch)
    );
    let mut old_shape = response(&input);
    old_shape.as_object_mut().ok_or("expected object")?.remove("questionSha256");
    assert_eq!(parse(&input, &old_shape).err(), Some(SemanticValidationError::InvalidJson));
    let mut value = response(&input);
    value["units"][0]["evidence"][0]["documentId"] = json!(Uuid::from_u128(99));
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidJson));
    for field in ["text", "span"] {
        let mut value = response(&input);
        value["units"][0][field] = json!("forged answer block");
        assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidJson));
    }
    Ok(())
}

#[test]
fn output_token_byte_link_and_caveat_caps_are_independent() -> TestResult {
    let input = prepared("A fact.")?;
    let value = response(&input).to_string();
    assert_eq!(
        input
            .parse_response(
                StructuredSemanticOutput {
                    json: &value,
                    status: SemanticOutputStatus::Complete,
                    output_tokens: Some(MAX_OUTPUT_TOKENS + 1)
                },
                1,
                None
            )
            .err(),
        Some(SemanticValidationError::OutputTooLarge)
    );
    let large = format!("{}{}", value, " ".repeat(MAX_OUTPUT_UTF8_BYTES));
    assert_eq!(
        input
            .parse_response(
                StructuredSemanticOutput {
                    json: &large,
                    status: SemanticOutputStatus::Complete,
                    output_tokens: Some(1)
                },
                1,
                None
            )
            .err(),
        Some(SemanticValidationError::OutputTooLarge)
    );
    let mut value = response(&input);
    value["units"][0]["evidence"] = json!(vec![quote(&input); MAX_LINKS_PER_UNIT + 1]);
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::AssessmentTooLarge));
    let mut value = response(&input);
    value["completeness"] = json!("missing_essential_caveats");
    value["missingCaveats"] = json!(vec![
        json!({"description":"A caveat.","evidence":[quote(&input)]});
        MAX_MISSING_CAVEATS + 1
    ]);
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::AssessmentTooLarge));
    value["missingCaveats"] = json!([{"description":"é".repeat(MAX_CAVEAT_UTF8_BYTES / 2 + 1),"evidence":[quote(&input)]}]);
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::AssessmentTooLarge));
    Ok(())
}

#[test]
fn factual_verdicts_and_missing_caveats_require_exact_evidence_links() -> TestResult {
    let input = prepared("A fact.")?;
    for support in ["supported", "mixed", "contradicted"] {
        let mut value = response(&input);
        value["units"][0]["support"] = json!(support);
        value["units"][0]["evidence"] = json!([]);
        assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::MissingEvidence));
    }
    let mut value = response(&input);
    value["completeness"] = json!("missing_essential_caveats");
    value["missingCaveats"] = json!([{"description":"Omission.","evidence":[]}]);
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::MissingEvidence));
    value["missingCaveats"][0]["evidence"] = json!([quote(&input)]);
    value["missingCaveats"][0]["evidence"][0]["quote"] = json!("forged");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidQuote));
    Ok(())
}

#[test]
fn operational_outcomes_have_no_semantic_assessment() -> TestResult {
    let input = prepared("A fact.")?;
    let not_run = input.not_run(SemanticNotRunReason::UnsupportedStructuredTransport, 0);
    assert!(matches!(not_run.outcome, SemanticVerificationOutcome::NotRun { .. }));
    for code in [
        SemanticFailureCode::Timeout,
        SemanticFailureCode::ProviderRefusal,
        SemanticValidationError::InvalidQuote.failure_code(),
    ] {
        let failed = input.failed(code, 5, Some(Uuid::from_u128(9)));
        assert!(matches!(failed.outcome, SemanticVerificationOutcome::Failed { .. }));
    }
    Ok(())
}

#[test]
fn complete_document_cannot_launder_an_absence_assertion_about_an_excerpt() -> TestResult {
    let input = prepare(
        "Q",
        "Neither source specifies the fact.",
        vec![complete_source("Whole document."), source("An excerpt.")],
    )?;
    let mut value = response(&input);
    let links: Vec<_> = input
        .sources()
        .iter()
        .map(|source| {
            json!({
                "sourceId":source.source_id,"quote":source.text
            })
        })
        .collect();
    value["units"][0]["assertsDocumentAbsence"] = json!(true);
    value["units"][0]["evidence"] = json!(links);
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::IncompleteDocumentScope));
    Ok(())
}

#[test]
fn unsupported_and_claimless_units_preserve_their_distinct_meanings() -> TestResult {
    let input = prepared("An unestablished assertion.")?;
    let mut value = response(&input);
    value["units"][0]["support"] = json!("insufficient_evidence");
    value["units"][0]["evidence"] = json!([]);
    value["completeness"] = json!("undetermined");
    let SemanticVerificationOutcome::Completed { assessment } = parse(&input, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_eq!(assessment.claim_support, SemanticClaimSupport::InsufficientEvidence);
    value["units"][0]["support"] = json!("no_factual_claims");
    value["responseClassification"] = json!("unresolved_refusal");
    assert!(parse(&input, &value).is_ok());
    value["units"][0]["assertsDocumentAbsence"] = json!(true);
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::IncoherentAssessment));
    value["units"][0]["assertsDocumentAbsence"] = json!(false);
    value["units"][0]["evidence"] = json!([quote(&input)]);
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::IncoherentAssessment));
    Ok(())
}

#[test]
fn correct_bounded_abstention_can_include_supported_context() -> TestResult {
    let input = prepare(
        "Q",
        "The source states a prerequisite.\nIt does not specify an instance identifier.",
        vec![complete_source("A neutral complete document.")],
    )?;
    let mut value = response(&input);
    value["responseClassification"] = json!("correct_source_bounded_abstention");
    value["units"][1]["assertsDocumentAbsence"] = json!(true);
    let SemanticVerificationOutcome::Completed { assessment } = parse(&input, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_eq!(
        assessment.response_classification,
        SemanticResponseClassification::CorrectSourceBoundedAbstention
    );
    assert_eq!(assessment.units[0].support, SemanticClaimSupport::Supported);
    Ok(())
}

#[test]
fn unique_cyrillic_quotes_resolve_to_public_utf8_byte_spans() -> TestResult {
    let prefix = "До: ёж.\n";
    let literal = "Проверка включена.";
    let text = format!("{prefix}{literal}\nПосле.");
    let input = prepare("Какое условие указано?", "Нейтральный ответ.", vec![source(&text)])?;
    let link = json!({"sourceId":input.sources()[0].source_id,"quote":literal});
    let mut value = response(&input);
    value["units"][0]["evidence"] = json!([link.clone()]);
    value["completeness"] = json!("missing_essential_caveats");
    value["missingCaveats"] =
        json!([{"description":"An essential condition was omitted.","evidence":[link]}]);
    let SemanticVerificationOutcome::Completed { assessment } = parse(&input, &value)?.outcome
    else {
        return Err("expected completed".into());
    };
    assert_ne!(prefix.len(), prefix.chars().count());
    for resolved in [&assessment.units[0].evidence[0], &assessment.missing_caveats[0].evidence[0]] {
        assert_eq!(resolved.span.start, prefix.len());
        assert_eq!(resolved.span.end, prefix.len() + literal.len());
        assert_eq!(text.get(resolved.span.start..resolved.span.end), Some(literal));
        assert_eq!(resolved.quote, literal);
    }
    value["units"][0]["evidence"][0]["quote"] = json!("Проверка  включена.");
    assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidQuote));
    Ok(())
}

#[test]
fn repeated_and_overlapping_quotes_are_ambiguous_for_units_and_caveats() -> TestResult {
    for (text, literal) in
        [("Начало. Повтор. Конец. Повтор.", "Повтор."), ("aaa", "aa"), ("ёжёжёж", "ёжёж")]
    {
        let input = prepare("Q", "A candidate answer.", vec![source(text)])?;
        assert!(parse(&input, &response(&input)).is_ok());
        let link = json!({"sourceId":input.sources()[0].source_id,"quote":literal});
        let mut value = response(&input);
        value["units"][0]["evidence"] = json!([link.clone()]);
        assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::AmbiguousQuote));
        let mut value = response(&input);
        value["completeness"] = json!("missing_essential_caveats");
        value["missingCaveats"] = json!([{"description":"Omission.","evidence":[link]}]);
        assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::AmbiguousQuote));
    }
    let input = prepared("A factual block.")?;
    let mut duplicate = response(&input);
    duplicate["units"][0]["evidence"] = json!([quote(&input), quote(&input)]);
    assert_eq!(parse(&input, &duplicate).err(), Some(SemanticValidationError::InvalidQuote));
    Ok(())
}

#[test]
fn provider_link_spans_and_authoritative_ids_are_rejected_including_caveats() -> TestResult {
    let input = prepared("A factual block.")?;
    for (field, supplied) in [
        ("span", json!({"start":0,"end":input.sources()[0].text.len()})),
        ("documentId", json!(Uuid::from_u128(1))),
        ("chunkId", json!(Uuid::from_u128(3))),
    ] {
        let mut value = response(&input);
        value["units"][0]["evidence"][0][field] = supplied.clone();
        assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidJson));
        let mut value = response(&input);
        value["completeness"] = json!("missing_essential_caveats");
        value["missingCaveats"] = json!([{"description":"Omission.","evidence":[quote(&input)]}]);
        value["missingCaveats"][0]["evidence"][0][field] = supplied;
        assert_eq!(parse(&input, &value).err(), Some(SemanticValidationError::InvalidJson));
    }
    Ok(())
}

#[test]
fn preparation_failure_reports_map_every_error_without_semantic_success() {
    use SemanticPreparationError as Error;
    use SemanticVerificationOutcome::{Failed, NotRun};

    let cases = [
        (Error::EmptyAnswer, NotRun { reason: SemanticNotRunReason::EmptyAnswer }),
        (Error::NoEvidence, NotRun { reason: SemanticNotRunReason::NoEvidence }),
        (Error::AnswerTooLarge, NotRun { reason: SemanticNotRunReason::InputLimitExceeded }),
        (Error::QuestionTooLarge, NotRun { reason: SemanticNotRunReason::InputLimitExceeded }),
        (Error::TooManyUnits, NotRun { reason: SemanticNotRunReason::InputLimitExceeded }),
        (Error::TooManySources, NotRun { reason: SemanticNotRunReason::InputLimitExceeded }),
        (Error::EvidenceTooLarge, NotRun { reason: SemanticNotRunReason::InputLimitExceeded }),
        (Error::InvalidSource, Failed { code: SemanticFailureCode::InvalidProvenance }),
        (
            Error::InvalidCompleteDocumentProof,
            Failed { code: SemanticFailureCode::InvalidProvenance },
        ),
        (Error::DuplicateSource, Failed { code: SemanticFailureCode::InvalidProvenance }),
    ];
    let question = " Question: ёж? ";
    let answer = " Raw candidate. ";
    for (error, expected) in cases {
        let report = preparation_failure_report(question, answer, error, 13);
        assert_eq!(report.outcome, expected);
        assert_eq!(report.question_sha256, preparation::sha256(question.as_bytes()));
        assert_eq!(report.checked_answer_sha256, preparation::sha256(answer.as_bytes()));
        assert_eq!(report.evidence_manifest_sha256, preparation::manifest_sha256(&[]));
        assert!(report.answer_units.is_empty());
        assert!(report.sources.is_empty());
        assert_eq!(report.provider_call_id, None);
        assert_eq!(report.visible_answer_sha256, None);
        assert_eq!(report.elapsed_ms, 13);
    }
}

#[test]
fn preparation_failure_reports_do_not_retain_oversized_answers_or_invalid_sources() -> TestResult {
    let question = "A question.";
    let answer = "x".repeat(MAX_ANSWER_UTF8_BYTES + 1);
    let error = prepare(question, &answer, vec![source("A selected source.")])
        .err()
        .ok_or("expected cap refusal")?;
    assert_eq!(error, SemanticPreparationError::AnswerTooLarge);
    let report = preparation_failure_report(question, &answer, error, 1);
    assert!(report.answer_units.is_empty());
    assert!(report.sources.is_empty());
    assert_eq!(report.checked_answer_sha256, preparation::sha256(answer.as_bytes()));
    assert!(serde_json::to_string(&report)?.len() < answer.len());

    let mut invalid = complete_source("Canonical text must not become verified evidence.");
    if let Some(proof) = &mut invalid.complete_document {
        proof.canonical_utf8_bytes += 1;
    }
    let error =
        prepare(question, "A candidate.", vec![invalid]).err().ok_or("expected proof refusal")?;
    assert_eq!(error, SemanticPreparationError::InvalidCompleteDocumentProof);
    let report = preparation_failure_report(question, "A candidate.", error, 2);
    assert_eq!(
        report.outcome,
        SemanticVerificationOutcome::Failed { code: SemanticFailureCode::InvalidProvenance }
    );
    assert_eq!(report.evidence_manifest_sha256, preparation::manifest_sha256(&[]));
    assert!(report.answer_units.is_empty());
    assert!(report.sources.is_empty());
    assert_eq!(report.provider_call_id, None);
    Ok(())
}
