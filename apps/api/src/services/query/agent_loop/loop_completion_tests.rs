use super::*;

#[test]
fn completed_nonterminal_answer_is_retained_only_for_exhausted_repair_fallback() {
    let structured_content = with_test_completion_envelope(serde_json::json!({
        "answerBody": "Completed tool answer.",
        "finalAnswerReady": false,
        "finalizable": false,
        "lifecycleState": "completed",
        "executionDetail": {
            "answerDisposition": "non_terminal",
            "verificationState": "insufficient_evidence"
        }
    }));
    let outcome = ToolExecutionOutcome {
        arguments_json: None,
        requested_arguments_json: None,
        message_content: String::new(),
        result_text: Some("  Completed tool answer.  ".to_string()),
        result_json: Some(serde_json::json!({
            "content": [{"type": "text", "text": "Completed tool answer."}],
            "structuredContent": structured_content,
            "isError": false
        })),
        grounding_text: None,
        grounded_answer_body: Some("Completed tool answer.".to_string()),
        grounded_answer_outcome: None,
        grounded_answer_ready: false,
        grounded_answer_completed: true,
        grounded_answer_needs_follow_up: true,
        grounded_answer_repair_reason: Some(GroundedAnswerRepairReason::VerificationIncomplete),
        grounded_answer_language: QueryLanguage::Auto,
        grounded_answer_clarification_required: false,
        is_error: false,
        is_replay: false,
        duration_ms: 0,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    };

    assert!(
        remember_completed_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome).is_none()
    );
    assert_eq!(
        remember_completed_nonterminal_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome,)
            .as_deref(),
        Some("Completed tool answer."),
    );
}

#[test]
fn verified_completion_partial_is_returnable_only_with_an_explicit_notice() {
    let structured_content = with_test_completion_envelope(serde_json::json!({
        "answerBody": "Подтверждённая часть ответа.",
        "finalAnswerReady": false,
        "finalizable": false,
        "lifecycleState": "completed",
        "completion": {
            "complete": false,
            "reason": "procedure_incomplete",
            "expected": 2,
            "observed": 0
        },
        "executionDetail": {
            "answerDisposition": "factual_ready",
            "verificationState": "verified",
            "verificationWarnings": []
        }
    }));
    let mut outcome = grounded_answer_ledger_outcome(serde_json::json!({
        "content": [{"type": "text", "text": "Подтверждённая часть ответа."}],
        "structuredContent": structured_content,
        "isError": false
    }));
    outcome.grounded_answer_repair_reason = Some(GroundedAnswerRepairReason::ProcedureIncomplete);

    let partial =
        remember_verified_partial_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
            .expect("verified completion partial");
    let visible = explicitly_mark_partial_grounded_answer(&partial, QueryLanguage::Ru);

    assert!(visible.starts_with("Ниже приведена только подтверждённая источниками часть"));
    assert!(visible.ends_with("Подтверждённая часть ответа."));

    let nonterminal_structured_content = with_test_completion_envelope(serde_json::json!({
        "answerBody": "Подтверждённая часть ответа.",
        "finalAnswerReady": false,
        "finalizable": false,
        "lifecycleState": "completed",
        "completion": {
            "complete": false,
            "reason": "procedure_incomplete",
            "expected": 2,
            "observed": 0
        },
        "executionDetail": {
            "answerDisposition": "non_terminal",
            "verificationState": "insufficient_evidence",
            "verificationWarnings": [{
                "code": "unsupported_literal",
                "message": "Unsupported literal"
            }]
        }
    }));
    outcome.result_json.as_mut().expect("result json")["structuredContent"] =
        nonterminal_structured_content;
    assert!(
        remember_verified_partial_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome,)
            .is_none()
    );
}

#[test]
fn completed_grounded_answer_fallback_requires_iteration_cap() {
    assert!(should_return_completed_grounded_answer_on_iteration_cap(
        AgentStopReason::IterationCap,
        1,
        Some("Completed tool answer.")
    ));
    assert!(!should_return_completed_grounded_answer_on_iteration_cap(
        AgentStopReason::Deadline,
        1,
        Some("Completed tool answer.")
    ));
    assert!(!should_return_completed_grounded_answer_on_iteration_cap(
        AgentStopReason::IterationCap,
        0,
        Some("Completed tool answer.")
    ));
    assert!(!should_return_completed_grounded_answer_on_iteration_cap(
        AgentStopReason::IterationCap,
        2,
        Some("Completed tool answer.")
    ));
    assert!(!should_return_completed_grounded_answer_on_iteration_cap(
        AgentStopReason::IterationCap,
        1,
        Some("   ")
    ));
}

#[test]
fn current_turn_iteration_cap_rejects_unrelated_prior_turn_without_a_current_answer() {
    let unrelated_prior_turn = ChatMessage::assistant_text(
        "Use `legacy-package`, `/etc/legacy.ini`, and `legacyTimeout`.".to_string(),
    );
    assert_eq!(unrelated_prior_turn.role, "assistant");

    let answer = current_turn_grounded_answer_on_iteration_cap(IterationCapAnswerInput {
        stopped_reason: AgentStopReason::IterationCap,
        incomplete_grounded_answer_needs_follow_up: false,
        focused_grounded_follow_up_attempted: false,
        successful_tool_call_count: 1,
        verified_grounded_answer_count: 0,
        last_verified_grounded_answer: None,
        last_completed_grounded_answer: None,
    });

    assert!(answer.is_none(), "an unrelated prior turn is not a current-turn answer source");
}

#[test]
fn current_turn_iteration_cap_uses_explicit_follow_up_verified_result() {
    let answer = current_turn_grounded_answer_on_iteration_cap(IterationCapAnswerInput {
        stopped_reason: AgentStopReason::IterationCap,
        incomplete_grounded_answer_needs_follow_up: false,
        focused_grounded_follow_up_attempted: true,
        successful_tool_call_count: 2,
        verified_grounded_answer_count: 1,
        last_verified_grounded_answer: Some("Current verified follow-up answer."),
        last_completed_grounded_answer: None,
    });

    assert_eq!(answer, Some("Current verified follow-up answer."));
}

#[test]
fn current_turn_iteration_cap_keeps_completed_result() {
    let answer = current_turn_grounded_answer_on_iteration_cap(IterationCapAnswerInput {
        stopped_reason: AgentStopReason::IterationCap,
        incomplete_grounded_answer_needs_follow_up: false,
        focused_grounded_follow_up_attempted: false,
        successful_tool_call_count: 1,
        verified_grounded_answer_count: 0,
        last_verified_grounded_answer: None,
        last_completed_grounded_answer: Some("Current completed answer."),
    });

    assert_eq!(answer, Some("Current completed answer."));
}

#[test]
fn verified_grounded_answer_fallback_waits_after_multi_tool_synthesis() {
    assert_eq!(verified_grounded_answer_fallback_candidate(Some("A0"), 1, 1), Some("A0"));
    assert_eq!(verified_grounded_answer_fallback_candidate(Some("A0"), 2, 2), None);
    assert_eq!(verified_grounded_answer_fallback_candidate(Some("A0"), 1, 2), None);
}

#[test]
fn final_answer_prefers_verified_union_when_parent_drops_multi_tool_anchors() {
    let first = "A: `A1`, `A2`, `A3`, and `A4`.";
    let second = "B: `B1`, `B2`, `B3`, and `B4`.";
    let union = format!("{first}\n\n{second}");
    let answer = "A: `A1` and `A2`. B: `B1`.".to_string();

    let guard = verified_grounded_answer_guard_candidate(Some(second), Some(&union), 2, 2)
        .expect("multi-tool verified guard");
    let finalized = finalize_agent_loop_answer(answer, "Q?", Some(guard), "req", Uuid::nil());

    assert_eq!(guard, union);
    assert_eq!(finalized, union);
}

#[test]
fn successful_tool_result_becomes_verifier_grounding() {
    let mut grounding = AssistantGroundingEvidence::default();
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Document read completed.".to_string(),
        }],
        structured_content: serde_json::json!({
            "documentTitle": "Alpha overview",
            "content": "The release channel is stable."
        }),
        is_error: false,
    };
    let evidence =
        tool_result_verification_text("read_document", &result).expect("verification text");

    push_tool_grounding_fragment(&mut grounding, "read_document", &evidence);

    assert_eq!(grounding.verification_corpus.len(), 1);
    assert!(grounding.verification_corpus[0].contains("read_document"));
    assert!(grounding.verification_corpus[0].contains("Document read completed."));
    assert!(grounding.verification_corpus[0].contains("The release channel is stable."));
    assert!(!grounding.verification_corpus[0].contains("\\\"content\\\""));
}

#[test]
fn inventory_tool_result_is_not_verifier_grounding() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Documents listed.".to_string(),
        }],
        structured_content: serde_json::json!({
            "items": [{
                "documentId": Uuid::now_v7(),
                "title": "Alpha overview",
                "readabilityState": "readable"
            }]
        }),
        is_error: false,
    };

    assert!(tool_result_verification_text("list_documents", &result).is_none());
}

#[test]
fn search_and_graph_results_can_be_verifier_grounding() {
    let search_result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Found Alpha overview.".to_string(),
        }],
        structured_content: serde_json::json!({
            "items": [{
                "documentTitle": "Alpha overview",
                "snippet": "The supported mode is standby."
            }]
        }),
        is_error: false,
    };
    let graph_result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Found related entities.".to_string(),
        }],
        structured_content: serde_json::json!({
            "entities": [{
                "label": "Alpha controller",
                "summary": "Controls standby mode."
            }]
        }),
        is_error: false,
    };

    let search_evidence =
        tool_result_verification_text("search_documents", &search_result).expect("search evidence");
    let graph_evidence =
        tool_result_verification_text("search_entities", &graph_result).expect("graph evidence");

    assert!(search_evidence.contains("The supported mode is standby."));
    assert!(graph_evidence.contains("Controls standby mode."));
}

#[test]
fn tool_error_result_is_not_verifier_grounding() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "read failed".to_string(),
        }],
        structured_content: serde_json::json!({
            "errorKind": "not_found",
            "message": "document not found"
        }),
        is_error: true,
    };

    assert!(tool_result_verification_text("read_document", &result).is_none());
}

#[test]
fn debug_tool_result_json_is_bounded() {
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "Large result completed.".to_string(),
        }],
        structured_content: serde_json::json!({
            "clarification": {
                "required": true,
                "question": "Which documented variant?"
            },
            "payload": "x".repeat(TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT + 128)
        }),
        is_error: false,
    };

    let debug_json = debug_tool_result_json(&result);

    assert_eq!(debug_json["isError"], false);
    assert_eq!(debug_json["content"][0]["text"], "Large result completed.");
    assert_eq!(debug_json["structuredContent"]["truncated"], true);
    assert_eq!(
        debug_json["structuredContent"]["clarification"],
        serde_json::json!({
            "required": true,
            "question": "Which documented variant?"
        })
    );
    assert!(
        debug_json["structuredContent"]["originalCharCount"].as_u64().unwrap()
            > TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT as u64
    );
}

#[test]
fn runtime_tool_answer_messages_preserve_chat_history_and_tool_result() {
    let history =
        vec![ChatMessage::user("first question"), ChatMessage::assistant_text("first answer")];

    let messages = build_runtime_tool_answer_messages(
        "system prompt".to_string(),
        &history,
        "continue",
        RUNTIME_RETRIEVED_CONTEXT_TOOL,
        serde_json::json!({ "question": "continue" }),
        "grounded context",
    );

    assert_eq!(
        messages.iter().map(|message| message.role.as_str()).collect::<Vec<_>>(),
        vec!["system", "user", "assistant", "user", "assistant", "tool"]
    );
    assert_eq!(messages[1].content.as_deref(), Some("first question"));
    assert_eq!(messages[2].content.as_deref(), Some("first answer"));
    assert_eq!(messages[4].tool_calls.len(), 1);
    assert_eq!(messages[4].tool_calls[0].name, RUNTIME_RETRIEVED_CONTEXT_TOOL);
    assert_eq!(messages[5].tool_call_id, Some(format!("call_{RUNTIME_RETRIEVED_CONTEXT_TOOL}")));
    assert_eq!(messages[5].content.as_deref(), Some("grounded context"));
}

#[test]
fn inline_context_answer_messages_carry_evidence_in_the_user_turn() {
    let history =
        vec![ChatMessage::user("first question"), ChatMessage::assistant_text("first answer")];

    let messages = build_inline_context_answer_messages(
        "system prompt".to_string(),
        &history,
        "continue",
        RUNTIME_RETRIEVED_CONTEXT_TOOL,
        "grounded context",
    );

    assert_eq!(
        messages.iter().map(|message| message.role.as_str()).collect::<Vec<_>>(),
        vec!["system", "user", "assistant", "user"]
    );
    assert!(messages.iter().all(|message| message.tool_calls.is_empty()));
    let expected = format!(
        "continue\n\n<{RUNTIME_RETRIEVED_CONTEXT_TOOL}>\ngrounded context\n</{RUNTIME_RETRIEVED_CONTEXT_TOOL}>"
    );
    assert_eq!(
        messages.last().and_then(|message| message.content.as_deref()),
        Some(expected.as_str())
    );
}
