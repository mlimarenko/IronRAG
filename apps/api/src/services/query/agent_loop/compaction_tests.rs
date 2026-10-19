use super::*;

#[test]
fn grounded_answer_tool_result_is_compact_for_model_messages() {
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
        "executionDetail": {
            "execution": {
                "id": execution_id,
                "runtimeExecutionId": runtime_execution_id
            },
            "verificationState": "verified",
            "verificationWarnings": [],
            "chunkReferences": (0..12)
                .map(|index| serde_json::json!({
                    "chunkId": Uuid::now_v7(),
                    "rank": index + 1,
                    "score": 1.0
                }))
                .collect::<Vec<_>>(),
            "preparedSegmentReferences": [],
            "technicalFactReferences": [],
            "entityReferences": [],
            "relationReferences": []
        }
    }));
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "grounded answer body".to_string(),
        }],
        structured_content,
        is_error: false,
    };

    let message = tool_result_model_message(GROUNDED_ANSWER_TOOL_NAME, &result);

    assert!(message.contains("grounded answer body"));
    assert!(message.contains("mustPreserveSpans"));
    assert!(message.contains("/etc/alpha.ini"));
    assert!(!message.contains("answerBody"));
    assert!(message.contains(&execution_id.to_string()));
    assert!(message.contains(&runtime_execution_id.to_string()));
    assert!(message.contains("\"referenceCounts\""));
    assert!(message.contains("\"chunkReferences\":12"));
    assert!(message.contains("\"omittedCount\":12"));
    assert!(!message.contains("\"rank\":1"));
    assert!(!message.contains("\"executionDetail\""));

    let verification_text =
        tool_result_verification_text(GROUNDED_ANSWER_TOOL_NAME, &result).unwrap();
    assert!(verification_text.contains("\"omittedCount\": 4"));
    assert!(verification_text.contains("\"rank\": 1"));
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
