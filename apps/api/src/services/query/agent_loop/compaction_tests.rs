use super::*;

/// The completion envelope with one twelve-reference execution detail, together
/// with the two execution ids the compaction must preserve.
fn compaction_fixture() -> (Value, Uuid, Uuid) {
    let execution_id = Uuid::now_v7();
    let runtime_execution_id = Uuid::now_v7();
    let structured_content = with_test_completion_envelope(serde_json::json!({
        "answerBody": "Use `/etc/alpha.ini`.",
        "executionId": execution_id,
        "runtimeExecutionId": runtime_execution_id,
        "conversationId": Uuid::now_v7(),
        "libraryId": Uuid::now_v7(),
        "workspaceId": Uuid::now_v7(),
        "lifecycleState": "completed",
        "finalAnswerReady": true,
        "finalizable": true,
        "mustPreserveSpans": ["/etc/alpha.ini"],
        "executionDetail": compaction_execution_detail(execution_id, runtime_execution_id),
    }));
    (structured_content, execution_id, runtime_execution_id)
}

/// One verified execution detail carrying twelve chunk references.
fn compaction_execution_detail(execution_id: Uuid, runtime_execution_id: Uuid) -> Value {
    serde_json::json!({
        "execution": {
            "id": execution_id,
            "runtimeExecutionId": runtime_execution_id
        },
        "verificationState": "verified",
        "verificationWarnings": [],
        "chunkReferences": compaction_chunk_references(),
        "preparedSegmentReferences": [],
        "technicalFactReferences": [],
        "entityReferences": [],
        "relationReferences": []
    })
}

/// Twelve chunk references whose rank follows their position.
fn compaction_chunk_references() -> Vec<Value> {
    (0..12)
        .map(|index| {
            serde_json::json!({
                "chunkId": Uuid::from_u128(index as u128 + 1),
                "rank": index + 1,
                "score": 1.0
            })
        })
        .collect()
}

#[test]
fn grounded_answer_tool_result_is_compact_for_model_messages() {
    let (structured_content, execution_id, runtime_execution_id) = compaction_fixture();
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "grounded answer body".to_string(),
        }],
        structured_content,
        is_error: false,
    };

    let message = tool_result_model_message(GROUNDED_ANSWER_TOOL_NAME, &result);
    assert_model_message_keeps_durable_spans(&message, execution_id, runtime_execution_id);
    assert_projected_references_are_bounded(&message);

    assert!(
        crate::interfaces::http::mcp::tools::source_evidence::admitted_source_texts(
            GROUNDED_ANSWER_TOOL_NAME,
            &result
        )
        .is_empty()
    );
}

/// The model message keeps the durable spans and ids and drops the raw payload.
fn assert_model_message_keeps_durable_spans(
    message: &str,
    execution_id: Uuid,
    runtime_execution_id: Uuid,
) {
    assert!(message.contains("grounded answer body"));
    assert!(message.contains("mustPreserveSpans"));
    assert!(message.contains("/etc/alpha.ini"));
    assert!(!message.contains("answerBody"));
    assert!(message.contains(&execution_id.to_string()));
    assert!(message.contains(&runtime_execution_id.to_string()));
    assert!(message.contains("\"referenceCounts\""));
    assert!(message.contains("\"chunkReferences\":12"));
    assert!(!message.contains("\"executionDetail\""));
}

/// The projected references stay bounded and report the omitted remainder.
fn assert_projected_references_are_bounded(message: &str) {
    let model_result: Value = serde_json::from_str(message).expect("model tool result");
    let references = model_result["structuredContent"]["references"]["chunkReferences"]
        .as_array()
        .expect("bounded chunk references");
    assert_eq!(references.len(), 9);
    assert_eq!(references[0]["chunkId"], serde_json::json!(Uuid::from_u128(1)));
    assert_eq!(references[0]["rank"], 1);
    assert_eq!(references[7]["rank"], 8);
    assert_eq!(references[8]["omittedCount"], 4);
}

#[test]
fn grounded_answer_model_projection_never_defaults_missing_completion_to_complete() {
    let structured_content = serde_json::json!({
        "answerBody": "Grounded text with missing completion metadata.",
        "executionId": Uuid::now_v7(),
        "lifecycleState": "completed",
        "executionDetail": {
            "verificationState": "verified",
            "verificationWarnings": []
        }
    });

    let compacted = compact_grounded_answer_structured_content_for_model(
        &structured_content,
        TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
    );

    assert_eq!(compacted["completion"], Value::Null);
    assert_eq!(compacted["finalAnswerReady"], serde_json::json!(false));
    assert_eq!(compacted["finalizable"], serde_json::json!(false));
}

#[test]
fn already_compact_grounded_answer_keeps_references_and_candidates_for_ui_agent() {
    let structured_content = with_test_completion_envelope(serde_json::json!({
        "responseProfile": "compact",
        "executionId": Uuid::now_v7(),
        "runtimeExecutionId": Uuid::now_v7(),
        "conversationId": Uuid::now_v7(),
        "libraryId": Uuid::now_v7(),
        "workspaceId": Uuid::now_v7(),
        "lifecycleState": "completed",
        "finalAnswerReady": false,
        "finalizable": false,
        "completion": {
            "complete": false,
            "reason": "procedure_incomplete",
            "expected": 2,
            "observed": 0
        },
        "verifier": {"state": "verified", "warningCount": 1},
        "warnings": [{"code": "procedure_incomplete", "message": "More steps required."}],
        "mustPreserveSpans": ["alphaMode"],
        "clarification": {"required": true, "question": "Which variant?"},
        "referenceSummary": {
            "totalCount": 2,
            "returnedCount": 2,
            "truncated": false,
            "references": [
                {"kind": "prepared_segment", "documentTitle": "Guide Alpha", "rank": 1},
                {"kind": "technical_fact", "displayValue": "alphaMode", "rank": 2}
            ]
        },
        "answerCandidateSummary": {
            "totalCount": 1,
            "returnedCount": 1,
            "truncated": false,
            "candidates": [{"label": "Variant Alpha", "kind": "document"}]
        }
    }));

    let compacted = compact_grounded_answer_structured_content_for_model(
        &structured_content,
        TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
    );

    assert_eq!(compacted["referenceSummary"]["totalCount"], serde_json::json!(2));
    assert_eq!(compacted["referenceSummary"]["returnedCount"], serde_json::json!(2));
    assert_eq!(compacted["referenceSummary"]["references"][0]["documentTitle"], "Guide Alpha");
    assert_eq!(compacted["answerCandidateSummary"]["returnedCount"], serde_json::json!(1));
    assert_eq!(compacted["answerCandidateSummary"]["candidates"][0]["label"], "Variant Alpha");
    assert_eq!(compacted["verificationState"], "verified");
}

#[test]
fn compact_projection_marks_upstream_attribution_when_sources_are_omitted() {
    let references = (0..8)
        .map(|index| {
            serde_json::json!({
                "kind": "chunk",
                "chunkId": format!("chunk-{index}"),
                "documentId": format!("document-{index}"),
                "revisionId": format!("revision-{index}"),
                "excerpt": format!("Supported passage {index}."),
                "claimUnitIds": [format!("claim-{index}")],
                "sourceIds": [format!("source-{index}")]
            })
        })
        .collect::<Vec<_>>();
    let attribution = serde_json::json!({
        "basis": "semantic_verification",
        "attributedUnitCount": 8,
        "projectedSourceCount": 8
    });
    let content = with_test_completion_envelope(serde_json::json!({
        "responseProfile": "compact",
        "answerBody": "Supported answer.",
        "finalAnswerReady": true,
        "clarification": {"required": false},
        "referenceSummary": {
            "totalCount": 8, "returnedCount": 8, "truncated": false,
            "attribution": attribution, "references": references
        }
    }));
    let compacted = compact_existing_grounded_answer_profile(
        &content,
        2,
        TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
    );
    let summary = &compacted["referenceSummary"];
    assert_eq!(summary["references"].as_array().expect("references").len(), 2);
    assert_eq!(summary["attribution"], attribution);
    assert_eq!(summary["attributionScope"], "upstream_reference_projection");
    assert_eq!(summary["secondaryTruncated"], true);
    assert_eq!(summary["attributionOmitted"], false);
    assert_eq!(summary["returnedCount"], 2);
}

#[test]
fn compact_grounded_source_attribution_survives_model_budget() {
    use crate::services::query::{
        assistant_grounding::AgentEvidenceManifest,
        execution::semantic_verification::{AuthorizedSemanticSource, EvidenceBudget, prepare},
    };
    use ironrag_contracts::assistant::semantic_verification::{
        SemanticAssessment, SemanticByteSpan, SemanticClaimSupport, SemanticCompleteness,
        SemanticEvidenceLink, SemanticNotRunReason, SemanticResponseClassification,
        SemanticSourceLocator, SemanticUnitVerdict, SemanticVerificationOutcome,
    };
    let chunk_id = Uuid::from_u128(101);
    let document_id = Uuid::from_u128(102);
    let revision_id = Uuid::from_u128(103);
    let text = "The release shipped the frontend, while the API remained planned.";
    let prepared = prepare(
        "What shipped?",
        text,
        vec![AuthorizedSemanticSource {
            document_id,
            document_revision_id: revision_id,
            locator: SemanticSourceLocator::Chunk { chunk_id },
            text: text.to_string(),
            complete_document: None,
        }],
    )
    .expect("canonical source preparation");
    let mut report = prepared.not_run(SemanticNotRunReason::Disabled, 0);
    let source_id = report.sources[0].source_id.clone();
    let unit_id = report.answer_units[0].unit_id.clone();
    report.outcome = SemanticVerificationOutcome::Completed {
        assessment: SemanticAssessment {
            claim_support: SemanticClaimSupport::Supported,
            completeness: SemanticCompleteness::Complete,
            response_classification: SemanticResponseClassification::FactualAnswer,
            missing_caveats: Vec::new(),
            units: vec![SemanticUnitVerdict {
                unit_id: unit_id.clone(),
                support: SemanticClaimSupport::Supported,
                asserts_document_absence: false,
                evidence: vec![SemanticEvidenceLink {
                    source_id: source_id.clone(),
                    span: SemanticByteSpan { start: 0, end: text.len() },
                    quote: text.to_string(),
                }],
            }],
        },
    };
    let mut budget = EvidenceBudget::new();
    let mut manifest = AgentEvidenceManifest::default();
    assert!(super::iteration::child_source_evidence_gate(Some(&report), Some(true)).is_some());
    assert_eq!(
        super::iteration::admit_child_report_sources(&mut budget, &mut manifest, &report),
        ChildReportAdmission::Admitted
    );
    assert_eq!(manifest.sources.len(), 1);
    assert_eq!(budget.measurement().source_entries, 1);
    assert_eq!(budget.measurement().evidence_utf8_bytes, report.sources[0].text.len());
    let reference = serde_json::json!({
        "kind": "chunk",
        "chunkId": chunk_id,
        "documentId": document_id,
        "revisionId": revision_id,
        "documentTitle": "Release notes",
        "excerpt": text,
        "claimUnitIds": [unit_id],
        "sourceIds": [source_id]
    });
    let attribution = serde_json::json!({
        "basis": "semantic_verification",
        "claimSupportAvailable": true,
        "claimUnitCount": 1,
        "attributedUnitCount": 1,
        "unattributedUnitCount": 0,
        "projectedSourceCount": 1,
        "semanticStatus": "completed",
        "semanticReason": null
    });
    let content = with_test_completion_envelope(serde_json::json!({
        "responseProfile": "compact",
        "answerBody": "The release shipped the frontend; the API was planned.",
        "finalAnswerReady": true,
        "clarification": {"required": false},
        "verifier": {"state": "verified"},
        "warnings": [{"code": "partial_coverage", "message": "x".repeat(12000)}],
        "referenceSummary": {
            "totalCount": 1, "returnedCount": 1, "truncated": false,
            "attribution": attribution, "references": [reference]
        }
    }));
    let compacted = compact_grounded_answer_structured_content_for_model(
        &content,
        TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
    );
    assert!(grounded_json_size_for_test(&compacted) <= TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT);
    assert_eq!(
        compacted["referenceSummary"]["attribution"],
        content["referenceSummary"]["attribution"]
    );
    assert_eq!(compacted["referenceSummary"]["references"][0], reference);
    assert_eq!(compacted["referenceSummary"]["returnedCount"], 1);
    assert_eq!(compacted["referenceSummary"]["attributionScope"], "upstream_reference_projection");
    assert_eq!(compacted["referenceSummary"]["secondaryTruncated"], true);
}

fn grounded_json_size_for_test(value: &Value) -> usize {
    serde_json::to_string(value).expect("serialize").chars().count()
}

#[test]
fn grounded_answer_model_projection_obeys_its_character_budget() {
    let huge = "x".repeat(5_000);
    let references = (0..20)
        .map(|rank| serde_json::json!({"rank": rank, "payload": huge.clone()}))
        .collect::<Vec<_>>();
    let preserve_spans = vec![huge.clone(); 16];
    let answer_candidates =
        (0..8).map(|_| serde_json::json!({"label": huge.clone()})).collect::<Vec<_>>();
    let warnings = (0..16)
        .map(|_| serde_json::json!({"code": "partial_coverage", "message": huge.clone()}))
        .collect::<Vec<_>>();
    let structured_content = with_test_completion_envelope(serde_json::json!({
        "answerBody": "Grounded partial answer.",
        "executionId": Uuid::now_v7(),
        "runtimeExecutionId": Uuid::now_v7(),
        "conversationId": Uuid::now_v7(),
        "libraryId": Uuid::now_v7(),
        "workspaceId": Uuid::now_v7(),
        "lifecycleState": "completed",
        "finalAnswerReady": false,
        "finalizable": false,
        "completion": {
            "complete": false,
            "reason": "procedure_incomplete",
            "expected": 2,
            "observed": 0
        },
        "mustPreserveSpans": preserve_spans,
        "answerCandidates": answer_candidates,
        "clarification": {"required": false},
        "executionDetail": {
            "verificationState": "verified",
            "verificationWarnings": warnings,
            "chunkReferences": references.clone(),
            "preparedSegmentReferences": references.clone(),
            "technicalFactReferences": references.clone(),
            "entityReferences": references.clone(),
            "relationReferences": references
        }
    }));

    let compacted = compact_grounded_answer_structured_content_for_model(
        &structured_content,
        TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
    );
    let serialized = serde_json::to_string(&compacted).expect("serialize compact projection");

    assert!(serialized.chars().count() <= TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT);
    assert_eq!(compacted["finalAnswerReady"], serde_json::json!(false));
    assert_eq!(compacted["repairPolicy"]["required"], serde_json::json!(true));
    assert_eq!(compacted["verificationState"], "verified");
}

#[test]
fn ui_agent_bounds_grounded_answer_tool_top_k_to_parent_turn() {
    let mut missing = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "focused subquestion"
    });
    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut missing,
        "focused subquestion",
        8,
        "workspace-a/library-b",
        &[],
    );
    assert_eq!(missing["topK"], 8);
    assert_eq!(missing["library"], "workspace-a/library-b");

    let mut wider = serde_json::json!({
        "library": "workspace-x/library-y",
        "query": "focused subquestion",
        "topK": 24
    });
    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut wider,
        "focused subquestion",
        8,
        "workspace-a/library-b",
        &[],
    );
    assert_eq!(wider["topK"], 8);
    assert_eq!(wider["library"], "workspace-a/library-b");

    let mut narrower = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "focused subquestion",
        "topK": 4
    });
    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut narrower,
        "focused subquestion",
        8,
        "workspace-a/library-b",
        &[],
    );
    assert_eq!(narrower["topK"], 8);
}

#[test]
fn ui_agent_enforces_shared_compact_profile_and_reference_ceiling() {
    let mut arguments = serde_json::json!({
        "query": "focused subquestion",
        "responseProfile": "full",
        "maxReferences": 64
    });

    normalize_agent_tool_argument_types(GROUNDED_ANSWER_TOOL_NAME, &mut arguments);
    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        "focused subquestion",
        24,
        "workspace-a/library-b",
        &[],
    );

    assert_eq!(arguments["responseProfile"], "compact");
    assert_eq!(
        arguments["maxReferences"],
        crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT
    );
}

#[test]
fn ui_agent_explicit_debug_request_uses_full_profile_without_compact_limit() {
    let mut arguments = serde_json::json!({
        "query": "focused subquestion",
        "includeDebug": true,
        "responseProfile": "compact",
        "maxReferences": 8
    });

    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        "focused subquestion",
        24,
        "workspace-a/library-b",
        &[],
    );

    assert_eq!(arguments["responseProfile"], "full");
    assert!(arguments.get("maxReferences").is_none());
}

#[test]
fn ui_agent_defaults_non_contextual_grounded_answer_top_k_to_configured_default() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "focused subquestion"
    });

    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        "focused subquestion",
        32,
        "workspace-a/library-b",
        &[],
    );

    assert_eq!(arguments["topK"], 24);
}

#[test]
fn ui_agent_raises_non_contextual_grounded_answer_tool_top_k_floor() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "focused subquestion",
        "topK": 5
    });

    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        "focused subquestion",
        32,
        "workspace-a/library-b",
        &[],
    );

    assert_eq!(arguments["topK"], 24);
}

#[test]
fn history_compaction_drops_failed_turn_payloads() {
    let failed_payload = serde_json::json!({
        "content": [{"type": "text", "text": "private failure detail"}],
        "structuredContent": {
            "errorKind": "invalid_mcp_tool_call",
            "message": "private failure detail"
        },
        "isError": true
    })
    .to_string();
    let prompt_history = vec![
        ChatMessage::user("Earlier neutral question"),
        ChatMessage::assistant_text(failed_payload.clone()),
        ChatMessage::assistant_text("Earlier verified answer"),
    ];
    let grounded_history = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: failed_payload,
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "x".repeat(
                crate::services::query::service::MAX_GROUNDED_ANSWER_TOOL_HISTORY_CHARS + 80,
            ),
        },
    ];

    let compacted_prompt = compact_agent_prompt_history(&prompt_history);
    let compacted_grounded = compact_grounded_answer_tool_history(&grounded_history);

    assert_eq!(compacted_prompt.len(), 2);
    assert_eq!(compacted_prompt[0].role, "user");
    assert_eq!(compacted_prompt[1].content.as_deref(), Some("Earlier verified answer"));
    assert_eq!(compacted_grounded.len(), 1);
    assert!(
        compacted_grounded[0].content_text.chars().count()
            <= crate::services::query::service::MAX_GROUNDED_ANSWER_TOOL_HISTORY_CHARS
    );
    assert!(
        !format!("{compacted_prompt:?}{compacted_grounded:?}").contains("private failure detail")
    );
}
