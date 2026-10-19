use super::*;

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
        grounding_text: None,
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
        grounding_text: None,
        grounded_answer_body: Some("Clean Alpha answer.".to_string()),
        grounded_answer_outcome: Some(AgentGroundedAnswerOutcome {
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
