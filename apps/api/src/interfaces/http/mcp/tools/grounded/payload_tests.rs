use chrono::Utc;
use ironrag_contracts::assistant::semantic_verification::{
    SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticAnswerUnit, SemanticAssessment,
    SemanticByteSpan, SemanticClaimSupport, SemanticCompleteness, SemanticEvidenceLink,
    SemanticEvidenceScope, SemanticNotRunReason, SemanticPreparationBound,
    SemanticPreparationLimit, SemanticResponseClassification, SemanticSourceEntry,
    SemanticSourceLocator, SemanticUnitVerdict, SemanticVerificationOutcome,
    SemanticVerificationReport,
};
use ironrag_contracts::assistant::{
    AssistantAnswerDisposition, AssistantChunkReference, AssistantClarification,
    AssistantExecution, AssistantExecutionDetail, AssistantPolicySummary,
    AssistantPreparedSegmentReference, AssistantRuntimeSummary, AssistantTechnicalFactReference,
    AssistantVerificationState,
};
use uuid::Uuid;

use super::*;

fn execution_id() -> Uuid {
    Uuid::from_u128(1)
}

fn base_detail() -> AssistantExecutionDetail {
    let now = Utc::now();
    AssistantExecutionDetail {
        context_bundle_id: Uuid::from_u128(2),
        execution: AssistantExecution {
            id: execution_id(),
            workspace_id: Uuid::from_u128(3),
            library_id: Uuid::from_u128(4),
            conversation_id: Uuid::from_u128(5),
            context_bundle_id: Uuid::from_u128(2),
            request_turn_id: None,
            response_turn_id: None,
            binding_id: None,
            runtime_execution_id: Uuid::from_u128(6),
            lifecycle_state: "completed".to_string(),
            active_stage: None,
            query_text: "Which source explains the failure?".to_string(),
            failure_code: None,
            started_at: now,
        },
        runtime_summary: AssistantRuntimeSummary {
            runtime_execution_id: Uuid::from_u128(6),
            lifecycle_state: "completed".to_string(),
            active_stage: None,
            turn_budget: 1,
            turn_count: 1,
            parallel_action_limit: 1,
            failure_code: None,
            failure_summary_redacted: None,
            policy_summary: AssistantPolicySummary {
                allow_count: 0,
                reject_count: 0,
                terminate_count: 0,
                recent_decisions: Vec::new(),
            },
            accepted_at: now,
            completed_at: Some(now),
        },
        runtime_stage_summaries: Vec::new(),
        request_turn: None,
        response_turn: None,
        chunk_references: Vec::new(),
        prepared_segment_references: Vec::new(),
        technical_fact_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        reference_summary: None,
        verification_state: AssistantVerificationState::NotRun,
        verification_warnings: Vec::new(),
        semantic_verification: None,
        answer_disposition: AssistantAnswerDisposition::FactualReady,
        clarification: AssistantClarification::default(),
    }
}

fn compact(detail: &AssistantExecutionDetail, limit: usize) -> Value {
    grounded_answer_tool_result_with_profile(
        "Grounded compact answer.",
        detail,
        McpGroundedAnswerResponseProfile::Compact,
        Some(limit),
    )
    .structured_content
}

fn chunk(chunk_id: Uuid, rank: i32, document: Option<(Uuid, &str)>) -> AssistantChunkReference {
    AssistantChunkReference {
        execution_id: execution_id(),
        chunk_id,
        revision_id: None,
        rank,
        score: 0.9,
        document_id: document.map(|(document_id, _)| document_id),
        document_title: document.map(|(_, title)| title.to_string()),
        section_path: vec!["Ops".to_string(), "Registry".to_string()],
        excerpt: Some("The registry owner must confirm what removed the blob.".to_string()),
    }
}

fn fact(
    fact_id: Uuid,
    revision_id: Uuid,
    rank: i32,
    value: &str,
) -> AssistantTechnicalFactReference {
    AssistantTechnicalFactReference {
        execution_id: execution_id(),
        fact_id,
        revision_id,
        fact_kind: "parameter_name".to_string(),
        normalized_value: value.to_string(),
        display_value: value.to_string(),
        rank,
        score: 0.8,
    }
}

fn segment(
    segment_id: Uuid,
    revision_id: Uuid,
    document_id: Uuid,
    rank: i32,
) -> AssistantPreparedSegmentReference {
    AssistantPreparedSegmentReference {
        execution_id: execution_id(),
        segment_id,
        revision_id,
        block_kind: "paragraph".to_string(),
        rank,
        score: 0.7,
        heading_trail: vec!["Ops".to_string()],
        section_path: vec!["Ops".to_string(), "Registry".to_string()],
        document_id: Some(document_id),
        document_title: Some("Registry runbook".to_string()),
        document_hint: None,
        source_access: None,
    }
}

fn source_entry(
    source_id: &str,
    document_id: Uuid,
    revision_id: Uuid,
    locator: SemanticSourceLocator,
    text: &str,
) -> SemanticSourceEntry {
    SemanticSourceEntry {
        source_id: source_id.to_string(),
        document_id,
        document_revision_id: revision_id,
        locator,
        scope: SemanticEvidenceScope::SelectedExcerpt,
        text_sha256: "a".repeat(64),
        text: text.to_string(),
    }
}

fn unit(unit_id: &str, evidence: Vec<(&str, &str)>) -> SemanticUnitVerdict {
    SemanticUnitVerdict {
        unit_id: unit_id.to_string(),
        support: if evidence.is_empty() {
            SemanticClaimSupport::InsufficientEvidence
        } else {
            SemanticClaimSupport::Supported
        },
        asserts_document_absence: false,
        evidence: evidence
            .into_iter()
            .map(|(source_id, quote)| SemanticEvidenceLink {
                source_id: source_id.to_string(),
                span: SemanticByteSpan { start: 0, end: quote.len() },
                quote: quote.to_string(),
            })
            .collect(),
    }
}

fn completed_report(
    sources: Vec<SemanticSourceEntry>,
    units: Vec<SemanticUnitVerdict>,
) -> SemanticVerificationReport {
    let answer_units = units
        .iter()
        .map(|unit| SemanticAnswerUnit {
            unit_id: unit.unit_id.clone(),
            span: SemanticByteSpan { start: 0, end: 1 },
            text: "x".to_string(),
        })
        .collect();
    SemanticVerificationReport {
        protocol_version: SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
        question_sha256: "b".repeat(64),
        checked_answer_sha256: "c".repeat(64),
        visible_answer_sha256: Some("c".repeat(64)),
        evidence_manifest_sha256: "d".repeat(64),
        answer_units,
        sources,
        elapsed_ms: 3,
        provider_call_id: None,
        preparation_limit: None,
        outcome: SemanticVerificationOutcome::Completed {
            assessment: SemanticAssessment {
                claim_support: SemanticClaimSupport::Supported,
                units,
                completeness: SemanticCompleteness::Complete,
                missing_caveats: Vec::new(),
                response_classification: SemanticResponseClassification::FactualAnswer,
            },
        },
    }
}

fn not_run_report(reason: SemanticNotRunReason) -> SemanticVerificationReport {
    SemanticVerificationReport {
        protocol_version: SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
        question_sha256: "b".repeat(64),
        checked_answer_sha256: "c".repeat(64),
        visible_answer_sha256: None,
        evidence_manifest_sha256: "d".repeat(64),
        answer_units: Vec::new(),
        sources: Vec::new(),
        elapsed_ms: 0,
        provider_call_id: None,
        preparation_limit: (reason == SemanticNotRunReason::InputLimitExceeded).then_some(
            SemanticPreparationLimit {
                bound: SemanticPreparationBound::SourceEntries,
                actual: 49,
                limit: 48,
            },
        ),
        outcome: SemanticVerificationOutcome::NotRun { reason },
    }
}

#[test]
fn compact_chunk_reference_exposes_document_section_and_excerpt() {
    let chunk_id = Uuid::from_u128(10);
    let document_id = Uuid::from_u128(20);
    let mut detail = base_detail();
    detail.chunk_references = vec![chunk(chunk_id, 1, Some((document_id, "Registry incident")))];
    detail.semantic_verification = Some(not_run_report(SemanticNotRunReason::InputLimitExceeded));

    let payload = compact(&detail, 8);
    let reference = &payload["referenceSummary"]["references"][0];

    assert_eq!(reference["kind"], "chunk");
    assert_eq!(reference["chunkId"], json!(chunk_id));
    assert_eq!(reference["documentId"], json!(document_id));
    assert_eq!(reference["documentTitle"], "Registry incident");
    assert_eq!(reference["sectionPath"], json!(["Ops", "Registry"]));
    assert!(reference["excerpt"].as_str().expect("excerpt").contains("registry owner"));
    let attribution = &payload["referenceSummary"]["attribution"];
    assert_eq!(attribution["basis"], "ranked_retrieval");
    assert_eq!(attribution["claimSupportAvailable"], false);
    assert_eq!(attribution["semanticStatus"], "not_run");
    assert_eq!(attribution["semanticReason"], "input_limit_exceeded");
}

#[test]
fn compact_reference_selection_keeps_card_metadata_from_filling_the_budget() {
    let mut detail = base_detail();
    let revision_id = Uuid::from_u128(40);
    detail.chunk_references =
        vec![chunk(Uuid::from_u128(41), 1, Some((Uuid::from_u128(42), "Playwright run")))];
    detail.technical_fact_references =
        ["Reporter", "Issue", "State", "Created", "Project", "Product", "Resolved"]
            .into_iter()
            .enumerate()
            .map(|(index, value)| fact(Uuid::from_u128(50 + index as u128), revision_id, 1, value))
            .collect();

    let payload = compact(&detail, 8);
    let summary = &payload["referenceSummary"];
    let references = summary["references"].as_array().expect("references");

    assert_eq!(summary["totalCount"], json!(8));
    assert_eq!(summary["returnedCount"], json!(3));
    assert_eq!(summary["truncated"], json!(true));
    assert_eq!(references[0]["kind"], "chunk");
    assert_eq!(
        references.iter().filter(|reference| reference["kind"] == "technical_fact").count(),
        2
    );
}

#[test]
fn compact_reference_selection_keeps_different_documents_before_metadata() {
    let document_a = Uuid::from_u128(60);
    let document_b = Uuid::from_u128(61);
    let revision_a = Uuid::from_u128(62);
    let mut detail = base_detail();
    detail.chunk_references =
        vec![chunk(Uuid::from_u128(63), 1, Some((document_a, "Card source")))];
    detail.prepared_segment_references =
        vec![segment(Uuid::from_u128(64), revision_a, document_b, 6)];
    detail.technical_fact_references =
        (0..5).map(|index| fact(Uuid::from_u128(70 + index), revision_a, 1, "State")).collect();

    let payload = compact(&detail, 4);
    let references = payload["referenceSummary"]["references"].as_array().expect("references");

    assert_eq!(references[0]["documentId"], json!(document_a));
    assert_eq!(references[1]["documentId"], json!(document_b));
    assert_eq!(references[1]["kind"], "prepared_segment");
    assert_eq!(
        references.iter().filter(|reference| reference["kind"] == "technical_fact").count(),
        2
    );
    assert_eq!(payload["referenceSummary"]["truncated"], json!(true));
}

#[test]
fn compact_fact_metadata_resolves_document_identity_from_its_revision() {
    let revision_id = Uuid::from_u128(80);
    let document_id = Uuid::from_u128(81);
    let mut detail = base_detail();
    detail.prepared_segment_references =
        vec![segment(Uuid::from_u128(82), revision_id, document_id, 2)];
    detail.technical_fact_references = vec![fact(Uuid::from_u128(83), revision_id, 1, "Resolved")];

    let payload = compact(&detail, 4);
    let fact = payload["referenceSummary"]["references"]
        .as_array()
        .expect("references")
        .iter()
        .find(|reference| reference["kind"] == "technical_fact")
        .expect("technical fact reference");

    assert_eq!(fact["revisionId"], json!(revision_id));
    assert_eq!(fact["documentId"], json!(document_id));
    assert_eq!(fact["documentTitle"], "Registry runbook");
}

#[test]
fn compact_claim_links_choose_evidence_for_substantive_units() {
    let chunk_a = Uuid::from_u128(90);
    let chunk_b = Uuid::from_u128(91);
    let revision_a = Uuid::from_u128(92);
    let revision_b = Uuid::from_u128(93);
    let document_a = Uuid::from_u128(94);
    let document_b = Uuid::from_u128(95);
    let mut detail = base_detail();
    detail.chunk_references = vec![
        chunk(chunk_a, 9, Some((document_a, "First claim source"))),
        chunk(chunk_b, 10, Some((document_b, "Second claim source"))),
    ];
    detail.chunk_references[0].revision_id = Some(revision_a);
    detail.chunk_references[1].revision_id = Some(revision_b);
    detail.technical_fact_references =
        vec![fact(Uuid::from_u128(96), Uuid::from_u128(97), 1, "Resolved")];
    detail.semantic_verification = Some(completed_report(
        vec![
            source_entry(
                "source-a",
                document_a,
                revision_a,
                SemanticSourceLocator::Chunk { chunk_id: chunk_a },
                "First claim quote",
            ),
            source_entry(
                "source-b",
                document_b,
                revision_b,
                SemanticSourceLocator::Chunk { chunk_id: chunk_b },
                "Second claim quote",
            ),
        ],
        vec![
            unit("unit-a", vec![("source-a", "First claim quote")]),
            unit("unit-b", vec![("source-b", "Second claim quote")]),
        ],
    ));

    let payload = compact(&detail, 3);
    let summary = &payload["referenceSummary"];
    let references = summary["references"].as_array().expect("references");

    assert_eq!(references[0]["chunkId"], json!(chunk_a));
    assert_eq!(references[0]["revisionId"], json!(revision_a));
    assert_eq!(references[0]["claimUnitIds"], json!(["unit-a"]));
    assert_eq!(references[0]["excerpt"], json!("First claim quote"));
    assert_eq!(references[1]["chunkId"], json!(chunk_b));
    assert_eq!(references[1]["claimUnitIds"], json!(["unit-b"]));
    assert_eq!(references[1]["excerpt"], json!("Second claim quote"));
    assert_eq!(summary["attribution"]["basis"], "semantic_verification");
    assert_eq!(summary["attribution"]["claimSupportAvailable"], true);
    assert_eq!(summary["attribution"]["claimUnitCount"], json!(2));
    assert_eq!(summary["attribution"]["attributedUnitCount"], json!(2));
}

#[test]
fn compact_claim_link_projects_authorized_document_slice_without_a_stored_reference() {
    let document_id = Uuid::from_u128(110);
    let revision_id = Uuid::from_u128(111);
    let mut detail = base_detail();
    detail.semantic_verification = Some(completed_report(
        vec![source_entry(
            "source-slice",
            document_id,
            revision_id,
            SemanticSourceLocator::DocumentSlice { start: 12, end: 40 },
            "The registry owner confirmed the deletion.",
        )],
        vec![unit(
            "unit-slice",
            vec![("source-slice", "The registry owner confirmed the deletion.")],
        )],
    ));

    let payload = compact(&detail, 4);
    let summary = &payload["referenceSummary"];
    let reference = &summary["references"][0];

    assert_eq!(summary["totalCount"], json!(1));
    assert_eq!(summary["returnedCount"], json!(1));
    assert_eq!(summary["truncated"], json!(false));
    assert_eq!(reference["kind"], "document_source");
    assert_eq!(reference["documentId"], json!(document_id));
    assert_eq!(reference["revisionId"], json!(revision_id));
    assert_eq!(reference["locator"]["kind"], "document_slice");
    assert_eq!(reference["scope"], "selected_excerpt");
    assert_eq!(reference["claimUnitIds"], json!(["unit-slice"]));
    assert_eq!(summary["attribution"]["projectedSourceCount"], json!(1));
}

#[test]
fn compact_claim_coverage_reports_units_without_visible_evidence() {
    let chunk_id = Uuid::from_u128(120);
    let document_id = Uuid::from_u128(121);
    let revision_id = Uuid::from_u128(122);
    let mut detail = base_detail();
    detail.chunk_references = vec![chunk(chunk_id, 1, Some((document_id, "Claim source")))];
    detail.semantic_verification = Some(completed_report(
        vec![source_entry(
            "source-a",
            document_id,
            revision_id,
            SemanticSourceLocator::Chunk { chunk_id },
            "Supported claim quote",
        )],
        vec![
            unit("unit-supported", vec![("source-a", "Supported claim quote")]),
            unit("unit-uncovered", Vec::new()),
        ],
    ));

    let payload = compact(&detail, 4);
    let attribution = &payload["referenceSummary"]["attribution"];

    assert_eq!(attribution["claimUnitCount"], json!(2));
    assert_eq!(attribution["attributedUnitCount"], json!(1));
    assert_eq!(attribution["unattributedUnitCount"], json!(1));
}

#[test]
fn compact_reference_limit_preserves_stable_chunk_identity() {
    let chunk_id = Uuid::from_u128(130);
    let document_id = Uuid::from_u128(131);
    let revision_id = Uuid::from_u128(132);
    let mut detail = base_detail();
    detail.chunk_references = vec![
        chunk(chunk_id, 1, Some((document_id, "Stable source"))),
        chunk(Uuid::from_u128(133), 2, Some((Uuid::from_u128(134), "Second source"))),
    ];
    detail.chunk_references[0].revision_id = Some(revision_id);
    detail.semantic_verification = Some(completed_report(
        vec![source_entry(
            "source-a",
            document_id,
            revision_id,
            SemanticSourceLocator::Chunk { chunk_id },
            "Stable claim quote",
        )],
        vec![unit("unit-a", vec![("source-a", "Stable claim quote")])],
    ));

    let one = compact(&detail, 1);
    let eight = compact(&detail, 8);
    let one_reference = &one["referenceSummary"]["references"][0];
    let eight_reference = &eight["referenceSummary"]["references"][0];

    assert_eq!(one["referenceSummary"]["returnedCount"], json!(1));
    assert_eq!(one_reference, eight_reference);
    assert_eq!(one_reference["chunkId"], json!(chunk_id));
    assert_eq!(one_reference["documentId"], json!(document_id));
    assert_eq!(one_reference["revisionId"], json!(revision_id));
}

#[test]
fn compact_chunk_revision_comes_from_the_chunk_not_from_semantic_sources() {
    let chunk_id = Uuid::from_u128(150);
    let document_id = Uuid::from_u128(151);
    let semantic_revision = Uuid::from_u128(152);
    let mut detail = base_detail();
    detail.chunk_references = vec![chunk(chunk_id, 1, Some((document_id, "Claim source")))];
    detail.semantic_verification = Some(completed_report(
        vec![source_entry(
            "source-a",
            document_id,
            semantic_revision,
            SemanticSourceLocator::Chunk { chunk_id },
            "Claim quote",
        )],
        vec![unit("unit-a", vec![("source-a", "Claim quote")])],
    ));

    let without_chunk_revision = compact(&detail, 4);
    let reference = &without_chunk_revision["referenceSummary"]["references"][0];
    assert_eq!(reference["revisionId"], Value::Null);
    assert_eq!(
        without_chunk_revision["referenceSummary"]["attribution"]["basis"],
        "semantic_verification"
    );

    let authoritative_revision = Uuid::from_u128(153);
    detail.chunk_references[0].revision_id = Some(authoritative_revision);
    let with_chunk_revision = compact(&detail, 4);
    assert_eq!(
        with_chunk_revision["referenceSummary"]["references"][0]["revisionId"],
        json!(authoritative_revision)
    );
}

#[test]
fn compact_reference_without_source_access_keeps_the_opaque_chunk_identity() {
    let chunk_id = Uuid::from_u128(140);
    let mut detail = base_detail();
    detail.chunk_references = vec![chunk(chunk_id, 1, None)];

    let payload = compact(&detail, 8);
    let reference = &payload["referenceSummary"]["references"][0];

    assert_eq!(reference["chunkId"], json!(chunk_id));
    assert_eq!(reference["documentId"], Value::Null);
    assert_eq!(reference["documentTitle"], Value::Null);
    assert_eq!(reference["revisionId"], Value::Null);
    assert!(reference["excerpt"].as_str().is_some());
}
