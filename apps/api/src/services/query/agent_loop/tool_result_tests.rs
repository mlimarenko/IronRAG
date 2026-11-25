use super::*;
use crate::services::query::assistant_grounding::AgentEvidenceManifest;
use crate::services::query::execution::semantic_verification::{EvidenceAdmission, EvidenceBudget};
use ironrag_contracts::assistant::semantic_verification::{
    SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticEvidenceScope, SemanticNotRunReason,
    SemanticSourceEntry, SemanticSourceLocator, SemanticVerificationOutcome,
    SemanticVerificationReport,
};

fn child_report(locator: SemanticSourceLocator, text_bytes: usize) -> SemanticVerificationReport {
    SemanticVerificationReport {
        protocol_version: SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
        question_sha256: "a".repeat(64),
        checked_answer_sha256: "b".repeat(64),
        visible_answer_sha256: None,
        evidence_manifest_sha256: "c".repeat(64),
        answer_units: Vec::new(),
        sources: vec![SemanticSourceEntry {
            source_id: "s1".to_string(),
            document_id: Uuid::from_u128(1),
            document_revision_id: Uuid::from_u128(2),
            locator,
            scope: SemanticEvidenceScope::SelectedExcerpt,
            text_sha256: "d".repeat(64),
            text: "x".repeat(text_bytes),
        }],
        elapsed_ms: 1,
        provider_call_id: None,
        preparation_limit: None,
        outcome: SemanticVerificationOutcome::NotRun { reason: SemanticNotRunReason::NoEvidence },
    }
}

fn native_document_read_result(
    content: &str,
    start: usize,
    total: usize,
) -> crate::interfaces::http::mcp::McpToolResult {
    let end = start + content.chars().count();
    crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Tool wrapper is not selected evidence".into(),
        }],
        structured_content: serde_json::json!({
            "documentId":Uuid::from_u128(1), "documentTitle":"Selected native document",
            "libraryId":Uuid::from_u128(2), "workspaceId":Uuid::from_u128(3),
            "latestRevisionId":Uuid::from_u128(4), "readMode":"full", "readabilityState":"readable",
            "readinessKind":"ready", "graphCoverageKind":"complete", "content":content,
            "sliceStartOffset":start, "sliceEndOffset":end, "totalContentLength":total,
            "hasMore":end < total, "sourceAccess":{"kind":"stored_document","href":"/documents/selected/source"}
        }),
        is_error: false,
    }
}

#[test]
fn native_document_read_identity_and_exact_text_survive_debug_compaction_and_replay() {
    let text = "Условие: без подтверждения перезапуск запрещён.";
    let mut result = native_document_read_result(text, 0, text.chars().count());
    result.structured_content["diagnostic"] =
        serde_json::json!("x".repeat(TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT));
    let reference =
        native_document_read_reference(READ_DOCUMENT_TOOL_NAME, &result).expect("native reference");
    assert_eq!(reference.document_id, Uuid::from_u128(1));
    assert_eq!(reference.revision_id, Some(Uuid::from_u128(4)));
    assert_eq!(reference.excerpt, text);
    assert_eq!(reference.slice_start_offset, 0);
    assert_eq!(reference.slice_end_offset, text.chars().count());
    assert_eq!(
        reference.source_access.as_ref().expect("source access").href,
        "/documents/selected/source"
    );
    let compact = debug_tool_result_json(&result);
    assert_eq!(compact["structuredContent"]["truncated"], true);
    let mut outcome = super::test_support::synthetic_success_outcome();
    outcome.document_reference = Some(Box::new(reference));
    outcome.result_json = Some(compact);
    let mut seen = BTreeMap::new();
    record_effective_tool_payload_outcome(&mut seen, "effective".into(), "raw".into(), &outcome);
    let entry = seen.remove("effective").expect("cached native result");
    let EffectiveToolPayloadState::Completed(payload) = entry.state else {
        panic!("completed payload")
    };
    let replay = replayed_tool_execution_outcome(*payload);
    assert!(replay.is_replay);
    let mut grounding = AssistantGroundingEvidence::default();
    let mut budget =
        crate::services::query::execution::semantic_verification::EvidenceBudget::new();
    assert!(retain_document_read_reference(
        &mut grounding,
        &mut budget,
        outcome.document_reference.as_deref()
    ));
    assert!(retain_document_read_reference(
        &mut grounding,
        &mut budget,
        replay.document_reference.as_deref()
    ));
    assert_eq!(grounding.document_references.len(), 1);
    assert_eq!(grounding.document_references[0].excerpt, text);
    assert_eq!(grounding.document_references[0].rank, 1);
}

#[test]
fn child_report_sources_are_all_or_nothing_against_the_shared_budget() {
    let mut budget = EvidenceBudget::new();
    let manifest = AgentEvidenceManifest::default();
    assert_eq!(budget.try_admit_chunk(Uuid::from_u128(10), 16_000), EvidenceAdmission::Retained);
    let before = budget.measurement();
    let mut trial_budget = budget.clone();
    let mut trial_manifest = manifest.clone();
    assert_eq!(
        super::iteration::admit_child_report_sources(
            &mut trial_budget,
            &mut trial_manifest,
            &child_report(SemanticSourceLocator::Chunk { chunk_id: Uuid::from_u128(11) }, 8_000),
        ),
        ChildReportAdmission::InadmissiblePresentation,
        "the parent byte budget is a presentation limit, not corrupt child data"
    );
    assert_eq!(budget.measurement(), before);
    assert!(trial_manifest.sources.is_empty());
    assert!(manifest.sources.is_empty());

    assert_eq!(
        super::iteration::admit_child_report_sources(
            &mut trial_budget,
            &mut trial_manifest,
            &child_report(SemanticSourceLocator::Segment { segment_id: Uuid::from_u128(12) }, 10),
        ),
        ChildReportAdmission::InadmissiblePresentation,
        "a segment source cannot be shown, and must never be skipped and shown anyway"
    );

    assert_eq!(
        super::iteration::admit_child_report_sources(
            &mut trial_budget,
            &mut trial_manifest,
            &child_report(SemanticSourceLocator::Chunk { chunk_id: Uuid::from_u128(13) }, 1_000),
        ),
        ChildReportAdmission::Admitted
    );
    assert!(trial_manifest.sources.iter().any(|source| source.chunk_id == Uuid::from_u128(13)));
    assert!(trial_budget.measurement().evidence_utf8_bytes > 16_000);

    // A refused child outcome must replace its full answer payload with the
    // omission marker before the next model-visible transcript is built.
    let mut child_outcome = super::test_support::synthetic_success_outcome();
    child_outcome.message_content = "child-answer-payload".to_string();
    assert!(!admitted_tool_message(&child_outcome, false).contains("child-answer-payload"));
    child_outcome.is_error = true;
    assert!(!admitted_tool_message(&child_outcome, false).contains("child-answer-payload"));
}

const PROBE_QUESTION: &str = "Куда уходит ответ сотрудника?";

/// One observed `grounded_answer` call plus the probe it did or did not
/// authorize.
struct GroundedCallObservation {
    state: super::state_machine::McpAgentLoopState,
    observed: ObservedAgentToolOutcomes,
    before_budget:
        crate::services::query::execution::semantic_verification::SelectedEvidenceMeasurement,
    probe: Option<ChatToolCall>,
}

fn observe_grounded_call(
    child_admission: ChildEvidenceAdmission,
    grounded_answer_completed: bool,
    is_error: bool,
    child_query_execution_ids: Vec<Uuid>,
) -> GroundedCallObservation {
    let mut state = super::state_machine::McpAgentLoopState::for_tests();
    let mut observed = ObservedAgentToolOutcomes {
        response_tool_calls: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
        iteration_made_progress: false,
    };
    let call = ChatToolCall {
        id: "call-1".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: "{}".to_string(),
    };
    let mut outcome = super::test_support::synthetic_success_outcome();
    outcome.child_query_execution_ids = child_query_execution_ids;
    outcome.message_content = "child secret payload".to_string();
    outcome.source_texts = vec!["child secret payload".to_string()];
    outcome.grounded_answer_completed = grounded_answer_completed;
    outcome.grounded_answer_needs_follow_up = true;
    outcome.grounded_answer_repair_reason =
        Some(GroundedAnswerRepairReason::VerificationIncomplete);
    outcome.grounded_answer_language = QueryLanguage::Ru;
    outcome.is_error = is_error;
    let before_budget = state.evidence_budget.measurement();
    observe_agent_tool_outcome(
        "request-1",
        Uuid::from_u128(9),
        &mut state,
        ObservedToolCall {
            iteration: 1,
            focused_follow_up_iteration: false,
            call: &call,
            outcome: &outcome,
            child_admission,
        },
        &mut observed,
    );
    let probe = state
        .focused_grounded_follow_up
        .requires_resolution()
        .then(|| state.focused_grounded_follow_up.take_call(PROBE_QUESTION, 8))
        .flatten();
    GroundedCallObservation { state, observed, before_budget, probe }
}

/// Accepted and presentation-only child evidence both owe the one repair their
/// valid envelope authorized: one probe, frozen question, `sequenceAttempt=1`.
#[test]
fn accepted_and_presentation_only_children_schedule_one_repair_probe() {
    let child_id = Uuid::from_u128(60);
    for admission in [
        ChildEvidenceAdmission::Admitted,
        ChildEvidenceAdmission::Refused(ChildEvidenceRefusal::InadmissibleSources),
    ] {
        let GroundedCallObservation { mut state, observed, before_budget, probe } =
            observe_grounded_call(admission, true, false, vec![child_id]);
        let probe = probe.as_ref().expect("authorized repair probe");
        assert_eq!(probe.name, GROUNDED_ANSWER_TOOL_NAME);
        let mut arguments: Value =
            serde_json::from_str(&probe.arguments_json).expect("repair arguments JSON");
        assert_eq!(arguments["query"], serde_json::json!(PROBE_QUESTION));
        assert_eq!(arguments["responseProfile"], serde_json::json!("compact"));
        normalize_agent_tool_argument_types(GROUNDED_ANSWER_TOOL_NAME, &mut arguments);
        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            AgentToolArgumentContext {
                user_question: PROBE_QUESTION,
                contextual_follow_up: false,
                grounded_top_k: 8,
                library_ref: "workspace-a/library-b",
                grounded_answer_tool_history: &[],
            },
        );
        assert_eq!(arguments["sequenceAttempt"], serde_json::json!(1));
        assert!(arguments.get(RUNTIME_REPAIR_ARGUMENT_FIELD).is_none());
        assert!(
            state.focused_grounded_follow_up.take_call(PROBE_QUESTION, 8).is_none(),
            "one repair probe per turn"
        );
        if matches!(admission, ChildEvidenceAdmission::Refused(_)) {
            assert_refusal_kept_accumulators_clean(&state, &observed, before_budget);
        } else {
            assert_eq!(state.successful_tool_call_count, 1);
            assert!(state.successful_tool_names.contains(GROUNDED_ANSWER_TOOL_NAME));
            assert_eq!(state.child_query_execution_ids, vec![child_id]);
            assert!(state.assistant_grounding.verification_corpus.is_empty());
        }
    }
}

/// Unverifiable child data, a malformed envelope and a missing child execution
/// id never authorize a probe.
#[test]
fn denied_child_outcomes_never_fund_a_repair_probe() {
    let child_id = Uuid::from_u128(61);

    let unverifiable = observe_grounded_call(
        ChildEvidenceAdmission::Refused(ChildEvidenceRefusal::UnverifiableChild),
        true,
        false,
        vec![child_id],
    );
    assert!(unverifiable.probe.is_none());
    assert_refusal_kept_accumulators_clean(
        &unverifiable.state,
        &unverifiable.observed,
        unverifiable.before_budget,
    );

    let malformed = observe_grounded_call(
        ChildEvidenceAdmission::Refused(ChildEvidenceRefusal::InadmissibleSources),
        false,
        false,
        vec![child_id],
    );
    assert!(malformed.probe.is_none());
    assert_refusal_kept_accumulators_clean(
        &malformed.state,
        &malformed.observed,
        malformed.before_budget,
    );

    let unidentified =
        observe_grounded_call(ChildEvidenceAdmission::NotApplicable, true, false, Vec::new());
    assert!(unidentified.probe.is_none());
    assert!(!unidentified.state.focused_grounded_follow_up.requires_resolution());

    let passthrough =
        observe_grounded_call(ChildEvidenceAdmission::Passthrough, true, false, vec![child_id]);
    assert!(passthrough.probe.is_none());

    let error = observe_grounded_call(
        ChildEvidenceAdmission::Refused(ChildEvidenceRefusal::UnverifiableChild),
        false,
        true,
        vec![child_id],
    );
    assert!(error.probe.is_none());
    assert_error_refusal_kept_accumulators_clean(&error.state, &error.observed);
}

/// A refused outcome leaves every model-visible accumulator untouched.
fn assert_refusal_kept_accumulators_clean(
    state: &super::state_machine::McpAgentLoopState,
    observed: &ObservedAgentToolOutcomes,
    before_budget: crate::services::query::execution::semantic_verification::SelectedEvidenceMeasurement,
) {
    let message = state.messages.last().expect("refusal message");
    assert!(!message.content.as_deref().unwrap_or_default().contains("child secret"));
    assert!(state.assistant_grounding.verification_corpus.is_empty());
    assert!(state.assistant_grounding.document_references.is_empty());
    assert!(!state.grounded_answer_evidence_ledger.has_guardable_evidence());
    assert_eq!(state.verified_grounded_answer_count, 0);
    assert!(state.last_verified_grounded_answer.is_none());
    assert_eq!(state.successful_tool_call_count, 0);
    assert!(state.successful_tool_names.is_empty());
    assert!(state.child_query_execution_ids.is_empty());
    assert!(observed.child_query_execution_ids.is_empty());
    assert_eq!(state.evidence_budget.measurement(), before_budget);
    assert!(state.evidence_manifest.sources.is_empty());
}

/// A refused error outcome leaks neither its payload nor its counters.
fn assert_error_refusal_kept_accumulators_clean(
    state: &super::state_machine::McpAgentLoopState,
    observed: &ObservedAgentToolOutcomes,
) {
    assert!(
        !state
            .messages
            .last()
            .and_then(|message| message.content.as_deref())
            .unwrap_or_default()
            .contains("child error secret")
    );
    assert!(!state.grounded_answer_evidence_ledger.has_guardable_evidence());
    assert_eq!(state.successful_tool_call_count, 0);
    assert!(state.child_query_execution_ids.is_empty());
    assert!(observed.child_query_execution_ids.is_empty());
}

#[test]
fn repeated_child_source_must_match_the_already_admitted_identity_and_text() {
    let mut budget = EvidenceBudget::new();
    let mut manifest = AgentEvidenceManifest::default();
    let report = child_report(SemanticSourceLocator::Chunk { chunk_id: Uuid::from_u128(20) }, 10);
    assert_eq!(
        super::iteration::admit_child_report_sources(&mut budget, &mut manifest, &report),
        ChildReportAdmission::Admitted
    );
    let before = budget.measurement();
    assert_eq!(
        super::iteration::admit_child_report_sources(&mut budget, &mut manifest, &report),
        ChildReportAdmission::Admitted
    );
    assert_eq!(budget.measurement(), before);
    assert_eq!(manifest.sources.len(), 1);

    let mut changed_text = report.clone();
    changed_text.sources[0].text.push('x');
    assert_eq!(
        super::iteration::admit_child_report_sources(&mut budget, &mut manifest, &changed_text),
        ChildReportAdmission::InvalidSource
    );
    let mut changed_revision = report.clone();
    changed_revision.sources[0].document_revision_id = Uuid::from_u128(99);
    assert_eq!(
        super::iteration::admit_child_report_sources(&mut budget, &mut manifest, &changed_revision),
        ChildReportAdmission::InvalidSource
    );

    // A presentation limit must not hide a later conflict with an admitted chunk.
    let mut hidden_conflict =
        child_report(SemanticSourceLocator::Segment { segment_id: Uuid::from_u128(30) }, 10);
    let mut duplicate = hidden_conflict.sources[0].clone();
    duplicate.source_id = "s2".to_string();
    duplicate.locator = SemanticSourceLocator::Chunk { chunk_id: Uuid::from_u128(20) };
    duplicate.text = "different text".to_string();
    hidden_conflict.sources.push(duplicate);
    assert_eq!(
        super::iteration::admit_child_report_sources(&mut budget, &mut manifest, &hidden_conflict),
        ChildReportAdmission::InvalidSource
    );

    assert_eq!(budget.measurement(), before);
    assert_eq!(manifest.sources[0].text, report.sources[0].text);
    assert_eq!(manifest.sources[0].document_revision_id, report.sources[0].document_revision_id);
}

#[test]
fn child_source_gate_rejects_missing_stale_or_unverifiable_reports() {
    let report = child_report(SemanticSourceLocator::Chunk { chunk_id: Uuid::from_u128(20) }, 10);
    assert!(super::iteration::child_source_evidence_gate(Some(&report), Some(true)).is_some());
    assert!(
        super::iteration::child_source_evidence_gate(None, Some(true)).is_none(),
        "missing child report must not pass"
    );
    assert!(
        super::iteration::child_source_evidence_gate(Some(&report), Some(false)).is_none(),
        "stale child sources must not pass"
    );
    assert!(
        super::iteration::child_source_evidence_gate(Some(&report), None).is_none(),
        "uncheckable currentness must not pass"
    );

    let empty = SemanticVerificationReport { sources: Vec::new(), ..report.clone() };
    assert!(
        super::iteration::child_source_evidence_gate(Some(&empty), Some(true)).is_none(),
        "a report without typed sources must not pass"
    );
    let mut budget = EvidenceBudget::new();
    let mut manifest = AgentEvidenceManifest::default();
    assert_eq!(
        super::iteration::admit_child_report_sources(
            &mut budget,
            &mut manifest,
            &child_report(SemanticSourceLocator::Chunk { chunk_id: Uuid::nil() }, 10),
        ),
        ChildReportAdmission::InvalidSource
    );
    assert_eq!(budget.measurement().source_entries, 0);
    assert!(manifest.sources.is_empty());
}

#[test]
fn repeated_source_text_leaves_room_for_a_distinct_excerpt() {
    let mut grounding = AssistantGroundingEvidence::default();
    let original = "а".repeat(TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT + 1);
    push_tool_source_text(&mut grounding, &original);
    let mut available = TOOL_GROUNDING_TOTAL_CHAR_LIMIT - TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT;
    let mut filler = 'a'..='z';
    while available > 4 {
        let length = (available - 4).min(TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT);
        let distinct = filler.next().unwrap().to_string().repeat(length);
        push_tool_source_text(&mut grounding, &distinct);
        available -= length;
    }
    push_tool_source_text(&mut grounding, &original);
    push_tool_source_text(&mut grounding, "факт");
    assert_eq!(grounding.verification_corpus.last().map(String::as_str), Some("факт"));
}

#[test]
fn document_read_admission_preserves_prior_and_suppresses_overflow_before_model() {
    use crate::services::query::execution::semantic_verification::{
        EvidenceAdmission, EvidenceBudget, MAX_SOURCE_ENTRIES,
    };
    let mut grounding = AssistantGroundingEvidence::default();
    let mut budget = EvidenceBudget::new();
    let result = native_document_read_result("admitted source", 0, 15);
    let reference =
        native_document_read_reference(READ_DOCUMENT_TOOL_NAME, &result).expect("source");
    let mut allowed = super::test_support::synthetic_success_outcome();
    allowed.document_reference = Some(Box::new(reference));
    allowed.message_content = "admitted source".into();
    assert!(admit_tool_grounding(&mut grounding, &mut budget, READ_DOCUMENT_TOOL_NAME, &allowed));
    assert_eq!(admitted_tool_message(&allowed, true), "admitted source");
    for index in 0..(MAX_SOURCE_ENTRIES - 1) {
        assert_eq!(
            budget.try_admit_block(Uuid::from_u128(100 + index as u128), 1),
            EvidenceAdmission::Retained
        );
    }
    assert!(admit_tool_grounding(&mut grounding, &mut budget, READ_DOCUMENT_TOOL_NAME, &allowed));
    assert_eq!(grounding.document_references.len(), 1);

    let result = native_document_read_result("rejected secret", 20, 100);
    let mut rejected = super::test_support::synthetic_success_outcome();
    rejected.document_reference =
        native_document_read_reference(READ_DOCUMENT_TOOL_NAME, &result).map(Box::new);
    rejected.message_content = "rejected secret".into();
    let admitted =
        admit_tool_grounding(&mut grounding, &mut budget, READ_DOCUMENT_TOOL_NAME, &rejected);
    assert!(!admitted);
    assert!(!admitted_tool_message(&rejected, admitted).contains("rejected secret"));
    assert!(grounding.verification_corpus.iter().all(|text| !text.contains("rejected secret")));
    assert_eq!(grounding.document_references[0].excerpt, "admitted source");
    rejected.document_reference = None;
    assert!(!admit_tool_grounding(&mut grounding, &mut budget, READ_DOCUMENT_TOOL_NAME, &rejected));
}

#[test]
fn partial_native_document_read_retains_only_the_actual_character_slice() {
    let text = "Условие";
    let result = native_document_read_result(text, 13, 100);
    let reference =
        native_document_read_reference(READ_DOCUMENT_TOOL_NAME, &result).expect("selected slice");
    assert_eq!(reference.slice_start_offset, 13);
    assert_eq!(reference.slice_end_offset, 20);
    assert_eq!(reference.excerpt, text);
    assert_ne!(reference.slice_end_offset, 100);
}

#[test]
fn failed_unversioned_malformed_or_wrapper_only_reads_cannot_supply_document_authority() {
    let result = native_document_read_result("Read content", 0, 12);
    assert!(native_document_read_reference("search_documents", &result).is_none());
    let mut failed = native_document_read_result("Read content", 0, 12);
    failed.is_error = true;
    assert!(native_document_read_reference(READ_DOCUMENT_TOOL_NAME, &failed).is_none());
    for (field, value) in [
        ("latestRevisionId", serde_json::Value::Null),
        ("sliceEndOffset", serde_json::json!(99)),
        ("content", serde_json::Value::Null),
        ("readabilityState", serde_json::json!("not_ready")),
    ] {
        let mut invalid = native_document_read_result("Read content", 0, 12);
        invalid.structured_content[field] = value;
        assert!(
            native_document_read_reference(READ_DOCUMENT_TOOL_NAME, &invalid).is_none(),
            "{field}"
        );
    }
    let mut wrapper = native_document_read_result("Read content", 0, 12);
    wrapper.structured_content = serde_json::Value::Null;
    wrapper.content[0].text =
        serde_json::to_string(&result.structured_content).expect("wrapper fixture");
    assert!(native_document_read_reference(READ_DOCUMENT_TOOL_NAME, &wrapper).is_none());
}

#[test]
fn passthrough_identity_comes_from_actual_execution_metadata_not_text_or_summary() {
    let actual = Uuid::from_u128(22);
    let unrelated = Uuid::from_u128(99);
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: format!("{{\"executionId\":\"{unrelated}\"}}"),
        }],
        structured_content: super::test_support::with_test_completion_envelope(serde_json::json!({
            "executionId":actual,"answerBody":"A child answer.","finalAnswerReady":true,"finalizable":true,"lifecycleState":"completed",
            "clarification":{"required":false},"executionDetail":{"answerDisposition":"factual_ready","semanticVerification":{"projection":"summary","executionId":unrelated}}
        })),
        is_error: false,
    };
    let outcome = grounded_answer_typed_outcome(GROUNDED_ANSWER_TOOL_NAME, &result)
        .expect("completed child outcome");
    assert_eq!(outcome.source_execution_id, Some(actual));
}

#[test]
fn tool_result_full_text_extracts_all_content_blocks() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![
            crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "First supported paragraph.".to_string(),
            },
            crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Second supported paragraph.".to_string(),
            },
        ],
        structured_content: serde_json::json!({
            "answerBody": "Ready answer.",
            "finalAnswerReady": true,
            "finalizable": true,
            "completion": {"complete": true},
            "lifecycleState": "completed",
            "executionDetail": {
                "verificationState": "verified"
            }
        }),
        is_error: false,
    };

    let answer = tool_result_full_text(&result.content).expect("answer");

    assert_eq!(answer, "First supported paragraph.\n\nSecond supported paragraph.");
}

#[test]
fn raw_result_text_is_never_remembered_as_an_answer() {
    // The rendered tool text also carries evidence blocks; before this
    // guard the selector served source dumps as the fallback answer
    // whenever the structured answerBody was missing or malformed.
    let rendered_tool_text = "### source_document.md\nraw evidence dump".to_string();
    let outcome = ToolExecutionOutcome {
        arguments_json: None,
        requested_arguments_json: None,
        message_content: String::new(),
        result_text: Some(rendered_tool_text),
        result_json: None,
        source_texts: Vec::new(),
        document_reference: None,
        grounded_answer_body: None,
        grounded_answer_outcome: None,
        grounded_answer_ready: true,
        grounded_answer_completed: true,
        grounded_answer_needs_follow_up: false,
        grounded_answer_repair_reason: None,
        grounded_answer_language: QueryLanguage::Auto,
        grounded_answer_clarification_required: false,
        is_error: false,
        is_replay: false,
        duration_ms: 0,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    };

    assert_eq!(
        remember_verified_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome),
        None,
        "an outcome without a structured answerBody has no answer"
    );
}

#[test]
fn verified_grounded_answer_memory_prefers_structured_answer_body() {
    let structured_content = with_test_completion_envelope(serde_json::json!({
        "answerBody": "Clean Alpha answer.",
        "finalAnswerReady": true,
        "finalizable": true,
        "lifecycleState": "completed",
        "executionDetail": {
            "verificationState": "verified"
        }
    }));
    let outcome = ToolExecutionOutcome {
        arguments_json: None,
        requested_arguments_json: None,
        message_content: String::new(),
        result_text: Some(
            "Clean Alpha answer.\n\nSources:\nAlpha source title\nBeta source title".to_string(),
        ),
        result_json: Some(serde_json::json!({
            "content": [{
                "type": "text",
                "text": "Clean Alpha answer.\n\nSources:\nAlpha source title\nBeta source title"
            }],
            "structuredContent": structured_content,
            "isError": false
        })),
        source_texts: Vec::new(),
        document_reference: None,
        grounded_answer_body: Some("Clean Alpha answer.".to_string()),
        grounded_answer_outcome: Some(AgentGroundedAnswerOutcome {
            source_execution_id: None,
            disposition: QueryAnswerDisposition::FactualReady,
            clarification: QueryClarification::default(),
        }),
        grounded_answer_ready: true,
        grounded_answer_completed: true,
        grounded_answer_needs_follow_up: false,
        grounded_answer_repair_reason: None,
        grounded_answer_language: QueryLanguage::Auto,
        grounded_answer_clarification_required: false,
        is_error: false,
        is_replay: false,
        duration_ms: 0,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    };

    let remembered = remember_verified_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
        .expect("remembered answer");
    let guard =
        remember_verified_grounded_answer_guard_text(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
            .expect("guard text");
    let completed = remember_completed_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
        .expect("completed answer");

    assert_eq!(remembered, "Clean Alpha answer.");
    assert_eq!(guard, "Clean Alpha answer.");
    assert_eq!(completed, "Clean Alpha answer.");
}

#[test]
fn source_marker_reformatting_keeps_good_synthesis_instead_of_raw_tool_body() {
    let verified = "3/3\n\n\
1. source=`Release 3.0.0`\n   Added Gamma support.\n\
2. source=`Release 2.0.0`\n   Added Beta support.\n\
3. source=`Release 1.0.0`\n   Added Alpha support.";
    let model_rewrite = "Latest records:\n\n\
1. Release 3.0.0 - Added Gamma support.\n\
2. Release 2.0.0 - Added Beta support.\n\
3. Release 1.0.0 - Added Alpha support.";

    let answer = finalize_agent_loop_answer(
        model_rewrite.to_string(),
        "list latest release records",
        Some(verified),
        "request-1",
        Uuid::now_v7(),
    );

    assert_eq!(answer, model_rewrite);
}

#[test]
fn finalization_fails_closed_when_synthesis_adds_an_unverified_structured_step() {
    let verified = "1. Δelta phase.\n2. Ωmega phase.";
    let model_rewrite = "1. Δelta phase.\n\
2. Ωmega phase.\n\
3. Sigma phase.";

    let answer = finalize_agent_loop_answer(
        model_rewrite.to_string(),
        "How do I configure the connector?",
        Some(verified),
        "request-1",
        Uuid::now_v7(),
    );

    assert_eq!(answer, verified);
}

#[test]
fn finalization_keeps_verified_grounded_identifier_inventory() {
    let verified = "\
Use module `alpha-connector`.
Config file: `/opt/alpha/modules/connector.conf`, section `[Main]`.
Parameters: `url`, `staticQrPayload`, `fillPaymentDetails`, `paymentDetails`.";
    let model_rewrite = "\
Use module `alpha-connector`.
Config file: `/opt/alpha/modules/connector.conf`, section `[Main]`.
Parameters: `url`, `staticQrPayload`.";

    let answer = finalize_agent_loop_answer(
        model_rewrite.to_string(),
        "configure Provider Alpha",
        Some(verified),
        "request-1",
        Uuid::now_v7(),
    );

    assert_eq!(answer, verified);
}

#[test]
fn final_grounded_answer_requires_ready_flag() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Verified but not explicitly final.".to_string(),
        }],
        structured_content: serde_json::json!({
            "lifecycleState": "completed",
            "executionDetail": {
                "verificationState": "verified",
                "execution": {
                    "lifecycleState": "completed"
                }
            }
        }),
        is_error: false,
    };

    assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
}

#[test]
fn unverified_grounded_answer_is_not_final_evidence() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "unsupported answer".to_string(),
        }],
        structured_content: serde_json::json!({
            "finalAnswerReady": false,
            "lifecycleState": "completed",
            "executionDetail": {
                "verificationState": "insufficient_evidence"
            }
        }),
        is_error: false,
    };

    assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
}

#[test]
fn completed_grounded_answer_without_verification_requires_follow_up() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Several matching documents were found. Please choose one.".to_string(),
        }],
        structured_content: serde_json::json!({
            "finalAnswerReady": false,
            "lifecycleState": "completed",
            "executionDetail": {
                "verificationState": "not_run"
            }
        }),
        is_error: false,
    };

    assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
}

#[test]
fn completed_grounded_answer_without_verification_state_requires_follow_up() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Completed answer with no verifier state.".to_string(),
        }],
        structured_content: serde_json::json!({
            "finalAnswerReady": false,
            "lifecycleState": "completed"
        }),
        is_error: false,
    };

    assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
}

#[test]
fn final_grounded_answer_readiness_rejects_unverified_completed_answer() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Completed answer with warnings.".to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "finalAnswerReady": false,
            "lifecycleState": "completed",
            "executionDetail": {
                "verificationState": "lenient"
            }
        })),
        is_error: false,
    };

    assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    assert!(grounded_answer_completed(GROUNDED_ANSWER_TOOL_NAME, &result));
    assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
}

#[test]
fn typed_grounded_answer_envelope_controls_final_readiness() {
    let ready_without_legacy_execution_detail = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Flag-only answer.".to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": "Flag-only answer.",
            "finalAnswerReady": true,
        })),
        is_error: false,
    };
    assert!(grounded_answer_ready(
        GROUNDED_ANSWER_TOOL_NAME,
        &ready_without_legacy_execution_detail,
    ));

    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Ready answer.".to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": "Ready answer.",
            "finalAnswerReady": true,
            "finalizable": true,
            "completion": {"complete": true},
            "lifecycleState": "completed",
            "executionDetail": {
                "verificationState": "verified"
            }
        })),
        is_error: false,
    };

    assert!(grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));

    let empty = crate::interfaces::http::mcp::McpToolResult {
        content: vec![],
        structured_content: serde_json::json!({
            "answerBody": "   ",
            "finalAnswerReady": true,
            "finalizable": true,
            "completion": {"complete": true},
            "lifecycleState": "completed",
            "executionDetail": {"verificationState": "verified"}
        }),
        is_error: false,
    };
    assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &empty));
}

#[test]
fn error_results_and_foreign_tools_are_never_ready_and_carry_no_envelope() {
    let valid_envelope = with_test_completion_envelope(serde_json::json!({
        "answerBody": "Ready answer.",
        "finalAnswerReady": true,
        "finalizable": true,
        "completion": {"complete": true},
        "lifecycleState": "completed",
        "executionDetail": {"verificationState": "verified"}
    }));
    let failed = crate::interfaces::http::mcp::McpToolResult {
        content: vec![],
        structured_content: valid_envelope.clone(),
        is_error: true,
    };

    assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &failed));
    assert!(
        grounded_answer_completion_envelope(&failed).is_none(),
        "an error result carries an error payload, not a completion envelope"
    );

    let foreign_tool = crate::interfaces::http::mcp::McpToolResult {
        content: vec![],
        structured_content: valid_envelope,
        is_error: false,
    };
    assert!(!grounded_answer_ready("knowledge_search", &foreign_tool));
}

#[test]
fn typed_safe_fallback_is_terminal_without_becoming_factual_ready() {
    let question = "Explain the documented behavior.";
    let answer_body = "Safe fallback selected by the typed producer.";
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: answer_body.to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": answer_body,
            "finalAnswerReady": false,
            "finalizable": false,
            "completion": {"complete": true},
            "lifecycleState": "completed",
            "executionDetail": {
                "answerDisposition": "safe_fallback",
                "verificationState": "not_run",
                "verificationWarnings": [{
                    "code": "semantic_verification_not_run",
                    "message": "The typed producer selected a terminal-safe fallback."
                }]
            }
        })),
        is_error: false,
    };

    assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
    assert!(!grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    assert_eq!(
        grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
        None,
        "a validated terminal safe fallback must not schedule a focused retry",
    );

    let call = ChatToolCall {
        id: "safe-fallback".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({"query": question}).to_string(),
    };
    let mut outcome = grounded_answer_ledger_outcome(debug_tool_result_json(&result));
    outcome.arguments_json = Some(call.arguments_json.clone());
    outcome.grounded_answer_body = Some(answer_body.to_string());
    outcome.grounded_answer_outcome =
        grounded_answer_typed_outcome(GROUNDED_ANSWER_TOOL_NAME, &result);
    let terminal = terminal_grounded_answer_nonfactual_candidate(
        std::slice::from_ref(&call),
        std::slice::from_ref(&outcome),
        question,
    )
    .expect("a fresh terminal result must not depend on previous tool-call counters");

    assert_eq!(terminal.answer, answer_body);
    assert_eq!(terminal.stop_reason, AgentStopReason::FinalAnswer);
}

#[test]
fn typed_final_readiness_still_blocks_unsupported_exact_literals() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Candidate with an unsupported exact literal.".to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": "Candidate with an unsupported exact literal.",
            "finalAnswerReady": false,
            "finalizable": false,
            "completion": {"complete": true},
            "lifecycleState": "completed",
            "executionDetail": {
                "answerDisposition": "non_terminal",
                "verificationState": "insufficient_evidence",
                "verificationWarnings": [{
                    "code": "unsupported_literal",
                    "message": "An exact literal is unsupported."
                }]
            }
        })),
        is_error: false,
    };

    assert!(!grounded_answer_ready_for_question(
        GROUNDED_ANSWER_TOOL_NAME,
        "Explain the documented behavior.",
        &result,
    ));
    assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
}

#[test]
fn final_grounded_answer_readiness_requires_complete_assessment() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Incomplete answer.".to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": "Incomplete answer.",
            "finalAnswerReady": true,
            "finalizable": true,
            "completion": {
                "complete": false,
                "reason": "procedure_incomplete",
                "expected": 2,
                "observed": 0
            },
            "lifecycleState": "completed",
            "executionDetail": {"verificationState": "verified"}
        })),
        is_error: false,
    };

    assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
}

#[test]
fn ready_passthrough_preserves_historical_and_absence_caveats() {
    let question = "What does the source establish?";
    let answer = "The historical source described `alpha`; it does not establish a prohibition.";
    let call = ChatToolCall {
        id: "call-grounded".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({"query": question}).to_string(),
    };
    let outcome = ready_grounded_answer_passthrough_outcome(answer, question);
    let result = grounded_answer_passthrough_candidate(&[call], &[outcome], question)
        .expect("verified answer should be passed through");
    assert_eq!(result.answer, answer);
}

#[test]
fn ready_grounded_answer_passthrough_is_independent_of_prior_global_tool_counters() {
    let user_question = "refresh Alpha Tool";
    let answer = "1. Run `alpha-tool --refresh`.\n\n```ini\n[Main]\nmode=true\n```";
    let call = ChatToolCall {
        id: "call-grounded".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({"query": "refresh Alpha Tool"}).to_string(),
    };
    let outcome = ready_grounded_answer_passthrough_outcome(answer, "refresh Alpha Tool");

    let passthrough = grounded_answer_passthrough_candidate(
        std::slice::from_ref(&call),
        std::slice::from_ref(&outcome),
        user_question,
    )
    .expect("one current fresh, complete, verified result should skip parent finalization");

    assert_eq!(passthrough.answer, answer);
    assert_eq!(
        passthrough.grounded_answer_outcome.disposition,
        QueryAnswerDisposition::FactualReady,
    );
}

#[test]
fn grounded_answer_passthrough_allows_only_outer_whitespace_and_line_endings() {
    let answer = "Authoritative answer.";
    let call = ChatToolCall {
        id: "call-grounded".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json:
            serde_json::json!({"query": "Refresh Alpha Tool\nusing the documented mode."})
                .to_string(),
    };
    let outcome = ready_grounded_answer_passthrough_outcome(
        answer,
        "  Refresh Alpha Tool\r\nusing the documented mode.  ",
    );

    assert_eq!(
        grounded_answer_passthrough_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&outcome),
            "Refresh Alpha Tool\nusing the documented mode.",
        )
        .as_ref()
        .map(|passthrough| passthrough.answer.as_str()),
        Some(answer),
    );
}

#[test]
fn grounded_answer_passthrough_preserves_literal_case_and_spacing() {
    let call = ChatToolCall {
        id: "call-grounded".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({"query": "Inspect `/opt/app.ini` after error \"A B\"."})
            .to_string(),
    };
    let case_changed = ready_grounded_answer_passthrough_outcome(
        "Candidate answer.",
        "Inspect `/opt/app.ini` after error \"A B\".",
    );
    assert!(
        grounded_answer_passthrough_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&case_changed),
            "Inspect `/Opt/App.ini` after error \"A B\".",
        )
        .is_none(),
        "case-sensitive path literals must compare exactly",
    );

    let spacing_changed = ready_grounded_answer_passthrough_outcome(
        "Candidate answer.",
        "Inspect `/opt/app.ini` after error \"A B\".",
    );
    assert!(
        grounded_answer_passthrough_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&spacing_changed),
            "Inspect `/opt/app.ini` after error \"A  B\".",
        )
        .is_none(),
        "spacing inside quoted literals must compare exactly",
    );
}

#[test]
fn grounded_answer_passthrough_rejects_narrowed_composite_query() {
    let user_question = "Compare Alpha and Beta across deployment behavior, then list every Gamma constraint and exception.";
    let executed_query =
        "Compare Alpha and Beta in detail across deployment behavior and runtime architecture.";
    let call = ChatToolCall {
        id: "call-grounded".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({"query": executed_query}).to_string(),
    };
    let outcome = ready_grounded_answer_passthrough_outcome(
        "Alpha and Beta differ in their deployment behavior.",
        executed_query,
    );

    assert!(
        grounded_answer_passthrough_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&outcome),
            user_question,
        )
        .is_none(),
        "a narrowed A/B answer must return to parent synthesis when the full request also asks for Gamma",
    );
}

#[test]
fn grounded_answer_passthrough_rejects_incomplete_or_repair_result() {
    let call = ChatToolCall {
        id: "call-grounded".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({"query": "configure Alpha Tool"}).to_string(),
    };
    let mut incomplete =
        ready_grounded_answer_passthrough_outcome("Partial answer.", "configure Alpha Tool");
    incomplete.grounded_answer_ready = false;
    assert!(
        grounded_answer_passthrough_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&incomplete),
            "configure Alpha Tool",
        )
        .is_none()
    );

    let mut repair =
        ready_grounded_answer_passthrough_outcome("Repair candidate.", "configure Alpha Tool");
    repair.grounded_answer_needs_follow_up = true;
    repair.grounded_answer_repair_reason = Some(GroundedAnswerRepairReason::ProcedureIncomplete);
    assert!(
        grounded_answer_passthrough_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&repair),
            "configure Alpha Tool",
        )
        .is_none()
    );

    let mut replay =
        ready_grounded_answer_passthrough_outcome("Cached answer.", "configure Alpha Tool");
    replay.is_replay = true;
    assert!(
        grounded_answer_passthrough_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&replay),
            "configure Alpha Tool",
        )
        .is_none()
    );
}

#[test]
fn grounded_answer_passthrough_rejects_multiple_tool_calls() {
    let calls = vec![
        ChatToolCall {
            id: "call-grounded".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({"query": "describe Alpha Tool"}).to_string(),
        },
        ChatToolCall {
            id: "call-document".to_string(),
            name: READ_DOCUMENT_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({"documentId": Uuid::nil()}).to_string(),
        },
    ];
    let outcomes = vec![
        ready_grounded_answer_passthrough_outcome("Ready answer.", "describe Alpha Tool"),
        synthetic_success_outcome(),
    ];

    assert!(
        grounded_answer_passthrough_candidate(&calls, &outcomes, "describe Alpha Tool",).is_none()
    );
}

#[test]
fn typed_nonterminal_disposition_blocks_even_when_verifier_says_verified() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Warning-bearing but verified answer.".to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": "Warning-bearing but verified answer.",
            "finalAnswerReady": false,
            "finalizable": false,
            "completion": {"complete": true},
            "lifecycleState": "completed",
            "executionDetail": {
                "answerDisposition": "non_terminal",
                "verificationState": "verified",
                "verificationWarnings": [
                    {
                        "code": "partial_coverage",
                        "warning": "Only part of the requested evidence was grounded."
                    }
                ]
            }
        })),
        is_error: false,
    };

    assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
}

#[test]
fn final_grounded_answer_with_non_evidence_warning_does_not_force_follow_up() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Warning-bearing verified answer.".to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": "Warning-bearing verified answer.",
            "finalAnswerReady": true,
            "finalizable": true,
            "completion": {"complete": true},
            "lifecycleState": "completed",
            "executionDetail": {
                "verificationState": "verified",
                "verificationWarnings": [
                    {
                        "code": "operator_note",
                        "warning": "Metadata-only note."
                    }
                ]
            }
        })),
        is_error: false,
    };

    assert!(grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    assert!(!grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
}
