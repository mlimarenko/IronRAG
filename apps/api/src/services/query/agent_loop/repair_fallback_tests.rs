use super::iteration::tool_failure_diagnostics::{
    model_tool_validation_failure, redacted_tool_failure_detail, tool_iteration_stop_reason,
};
use super::*;

#[test]
fn focused_follow_up_stays_effectively_non_identical_after_ui_normalization() {
    let question = "How do I configure the connector?";
    let initial = required_grounded_answer_tool_call(question, 24);
    let focused = focused_grounded_answer_follow_up_call(
        question,
        24,
        RuntimeGroundedRepairMetadata::from_reason(
            GroundedAnswerRepairReason::ProcedureIncomplete,
            QueryLanguage::En,
        ),
    );
    let initial_fingerprint = effective_tool_call_fingerprint_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &initial.arguments_json,
        AgentToolArgumentContext {
            user_question: question,
            contextual_follow_up: false,
            grounded_top_k: 24,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &[],
        },
    )
    .expect("initial fingerprint");
    let focused_fingerprint = effective_tool_call_fingerprint_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &focused.arguments_json,
        AgentToolArgumentContext {
            user_question: question,
            contextual_follow_up: false,
            grounded_top_k: 24,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &[],
        },
    )
    .expect("focused fingerprint");
    let mut normalized =
        serde_json::from_str::<Value>(&focused.arguments_json).expect("focused arguments");
    normalize_agent_tool_argument_types(GROUNDED_ANSWER_TOOL_NAME, &mut normalized);
    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut normalized,
        AgentToolArgumentContext {
            user_question: question,
            contextual_follow_up: false,
            grounded_top_k: 24,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &[],
        },
    );

    assert_ne!(initial_fingerprint, focused_fingerprint);
    assert_eq!(normalized["query"].as_str(), Some(question));
    assert_eq!(normalized["topK"], serde_json::json!(32));
    assert!(normalized.get(RUNTIME_REPAIR_ARGUMENT_FIELD).is_none());
}

#[test]
fn maximum_initial_breadth_reserves_one_candidate_for_a_fresh_repair_probe() {
    let question = "How do I configure the connector?";
    let initial = required_grounded_answer_tool_call(question, MAX_TOP_K);
    let focused = focused_grounded_answer_follow_up_call(
        question,
        MAX_TOP_K,
        RuntimeGroundedRepairMetadata::from_reason(
            GroundedAnswerRepairReason::ProcedureIncomplete,
            QueryLanguage::En,
        ),
    );
    let initial_arguments =
        serde_json::from_str::<Value>(&initial.arguments_json).expect("initial arguments");
    let focused_arguments =
        serde_json::from_str::<Value>(&focused.arguments_json).expect("focused arguments");

    assert_eq!(initial_arguments["query"].as_str(), Some(question));
    assert_eq!(focused_arguments["query"].as_str(), Some(question));
    assert_eq!(initial_arguments["topK"], serde_json::json!(MAX_TOP_K - 1));
    assert_eq!(focused_arguments["topK"], serde_json::json!(MAX_TOP_K));
}

#[test]
fn focused_follow_up_uses_runtime_dispatch_instead_of_the_model() {
    let call = focused_grounded_answer_follow_up_call(
        "How do I configure the connector?",
        24,
        RuntimeGroundedRepairMetadata::from_reason(
            GroundedAnswerRepairReason::ProcedureIncomplete,
            QueryLanguage::En,
        ),
    );

    let response =
        runtime_enforced_tool_response(call.clone(), "synthetic-provider", "synthetic-model");

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, GROUNDED_ANSWER_TOOL_NAME);
    assert_eq!(response.tool_calls[0].arguments_json, call.arguments_json);
    assert_eq!(response.usage_json["runtimeEnforced"], serde_json::json!(true));
}

#[test]
fn initial_grounded_answer_uses_runtime_dispatch_before_the_model() {
    let call =
        initial_grounded_answer_tool_call(1, 0, "How do I configure the sample connector?", 24)
            .expect("first iteration must use the authoritative answer tool");
    let arguments = serde_json::from_str::<Value>(&call.arguments_json).expect("arguments");

    assert_eq!(call.name, GROUNDED_ANSWER_TOOL_NAME);
    assert_eq!(arguments["query"], "How do I configure the sample connector?");
    assert_eq!(arguments["responseProfile"], "compact");
    assert_eq!(arguments["maxReferences"], serde_json::json!(8));
    assert!(initial_grounded_answer_tool_call(2, 1, "Q?", 24).is_none());
}

#[test]
fn failed_runtime_initial_grounded_answer_stops_before_a_model_retry() {
    let call = required_grounded_answer_tool_call("How do I configure it?", 24);
    let outcome = tool_execution_error("authoritative answer timed out");

    assert!(runtime_initial_grounded_answer_failed(
        true,
        std::slice::from_ref(&call),
        std::slice::from_ref(&outcome),
        false,
    ));
    assert!(!runtime_initial_grounded_answer_failed(
        false,
        std::slice::from_ref(&call),
        std::slice::from_ref(&outcome),
        false,
    ));
    assert!(!runtime_initial_grounded_answer_failed(true, &[call], &[outcome], true,));
}

#[test]
fn model_generated_malformed_call_can_be_corrected_on_the_next_iteration() {
    let validation_error = "invalid grounded_answer arguments: missing field `query`";
    let call = ChatToolCall {
        id: "call-malformed".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({"unexpectedField": "private argument value"})
            .to_string(),
    };
    let malformed = invalid_mcp_tool_call_outcome(validation_error);

    assert_eq!(
        tool_iteration_stop_reason(
            AgentToolCallOrigin::Model,
            true,
            std::slice::from_ref(&call),
            std::slice::from_ref(&malformed),
            false,
        ),
        None,
    );
    assert!(malformed.message_content.contains(validation_error));

    let corrected = synthetic_success_outcome();
    assert_eq!(
        tool_iteration_stop_reason(
            AgentToolCallOrigin::Model,
            false,
            std::slice::from_ref(&call),
            std::slice::from_ref(&corrected),
            false,
        ),
        None,
    );
}

#[test]
fn malformed_tool_diagnostic_contains_shape_without_values() {
    let private_value = "private argument value";
    let call = ChatToolCall {
        id: "call-malformed".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({
            "unexpectedField": private_value,
            "topK": [1, 2]
        })
        .to_string(),
    };
    let schema = serde_json::json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": {"type": "string"},
            "topK": {"type": "integer"}
        }
    });
    let outcome =
        invalid_mcp_tool_call_outcome("invalid grounded_answer arguments: missing field `query`");

    let diagnostic = model_tool_validation_failure(&call, &schema, &outcome)
        .expect("invalid MCP arguments must produce a redacted diagnostic");
    let rendered = diagnostic.to_string();

    assert!(rendered.contains("invalid_mcp_tool_call"));
    assert!(rendered.contains("unexpectedField:string"));
    assert!(rendered.contains("topK:array"));
    assert!(rendered.contains("missing:query"));
    assert!(rendered.contains("unexpected:unexpectedField"));
    assert!(rendered.contains("type:topK:array->integer"));
    assert!(!rendered.contains(private_value));
}

#[test]
fn runtime_initial_failure_detail_reports_codes_without_content() {
    let private_content = "private question and provider response";
    let mut outcome = tool_execution_error(private_content);
    outcome.result_json = Some(serde_json::json!({
        "content": [{"type": "text", "text": private_content}],
        "structuredContent": {
            "errorKind": "query_execution_failed",
            "failureCode": "query_agent_loop_failed",
            "message": private_content,
            "retryable": true,
            "repairHint": "retry_same_request"
        },
        "isError": true
    }));

    let detail = redacted_tool_failure_detail(Some(&outcome));

    assert_eq!(detail.error_kind, Some("query_execution_failed"));
    assert_eq!(detail.failure_code, Some("query_agent_loop_failed"));
    assert_eq!(detail.retryable, Some(true));
    assert_eq!(detail.repair_hint, Some("retry_same_request"));
    assert!(!format!("{detail:?}").contains(private_content));
}

#[test]
fn runtime_initial_failure_detail_rejects_content_shaped_codes() {
    let mut outcome = tool_execution_error("private content");
    outcome.result_json = Some(serde_json::json!({
        "structuredContent": {
            "errorKind": "private content must not become a log field",
            "failureCode": "UPPERCASE_PRIVATE_CONTENT",
            "repairHint": "retry_same_request"
        },
        "isError": true
    }));

    let detail = redacted_tool_failure_detail(Some(&outcome));

    assert_eq!(detail.error_kind, None);
    assert_eq!(detail.failure_code, None);
    assert_eq!(detail.repair_hint, Some("retry_same_request"));
}

#[test]
fn focused_follow_up_carries_typed_language_and_inventory_counts_without_query_prose() {
    let question = "Вопрос с отступами ";
    let call = focused_grounded_answer_follow_up_call(
        question,
        24,
        RuntimeGroundedRepairMetadata::from_reason(
            GroundedAnswerRepairReason::OrderedInventory { expected: 5, observed: 2 },
            QueryLanguage::Ru,
        ),
    );
    let arguments = serde_json::from_str::<Value>(&call.arguments_json).expect("arguments");

    assert_eq!(arguments["query"].as_str(), Some(question));
    assert_eq!(
        arguments[RUNTIME_REPAIR_ARGUMENT_FIELD],
        serde_json::json!({
            "reason": "ordered_inventory_incomplete",
            "language": "ru",
            "expected": 5,
            "observed": 2,
        })
    );
    assert_eq!(arguments["responseProfile"], "compact");
    assert_eq!(
        arguments["maxReferences"],
        crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT
    );
}

#[test]
fn unresolved_focused_repair_prefers_saved_grounded_fallback() {
    assert_eq!(
        unresolved_focused_follow_up_action(true, true),
        UnresolvedFocusedFollowUpAction::ReturnSavedFallback
    );
}

#[test]
fn unresolved_focused_repair_synthesizes_only_from_grounded_evidence() {
    assert_eq!(
        unresolved_focused_follow_up_action(false, true),
        UnresolvedFocusedFollowUpAction::SynthesizeFromEvidence
    );
    assert_eq!(
        unresolved_focused_follow_up_action(false, false),
        UnresolvedFocusedFollowUpAction::FailClosed
    );
}

#[test]
fn failed_focused_follow_up_remains_unresolved() {
    let mut state = FocusedGroundedFollowUpState::default();
    assert!(state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::Auto,));
    assert!(state.take_call("How do I configure it?", 24).is_some());

    state.observe_attempt_outcome(&tool_execution_error("repair timed out"));

    assert!(state.requires_resolution());
    assert!(state.is_unresolved_after_attempt());
}

#[test]
fn repeated_incomplete_focused_follow_up_remains_unresolved() {
    let mut state = FocusedGroundedFollowUpState::default();
    assert!(state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::Auto,));
    assert!(state.take_call("How do I configure it?", 24).is_some());
    let mut outcome = synthetic_success_outcome();
    outcome.grounded_answer_completed = true;
    outcome.grounded_answer_needs_follow_up = true;
    outcome.grounded_answer_repair_reason = Some(GroundedAnswerRepairReason::ProcedureIncomplete);

    state.observe_attempt_outcome(&outcome);

    assert!(state.requires_resolution());
    assert!(state.is_unresolved_after_attempt());
    assert!(!state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::Auto,));
}

#[tokio::test]
async fn rate_limit_past_deadline_uses_non_displacing_degraded_wrapper() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::RETRY_AFTER,
        "60".parse().expect("retry-after header should parse"),
    );
    let mut attempts = 0_u32;

    let (answer, telemetry) = crate::integrations::retry::with_provider_retry_execution(
        Duration::from_millis(10),
        async {
            let error = crate::integrations::retry::with_retry(
                || {
                    attempts = attempts.saturating_add(1);
                    std::future::ready(Err::<(), _>(
                        crate::integrations::retry::ProviderCallError::http_status(
                            "provider-alpha",
                            reqwest::StatusCode::TOO_MANY_REQUESTS,
                            &headers,
                            "rate limited",
                        ),
                    ))
                },
                crate::integrations::retry::RetryPolicy::idempotent_provider_call(
                    1,
                    Duration::ZERO,
                    Duration::from_millis(1),
                ),
            )
            .await
            .expect_err("retry-after cannot fit inside the execution deadline");
            let mut state = FocusedGroundedFollowUpState::default();
            assert!(
                state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::En)
            );
            assert!(state.take_call("How do I configure it?", 24).is_some());
            state.observe_attempt_outcome(&tool_execution_error(error.to_string()));

            let answer = mark_unresolved_repair_synthesis(
                &state,
                "Required literal stays first.".to_string(),
            );
            let telemetry = crate::integrations::retry::current_provider_retry_telemetry();
            (answer, telemetry)
        },
    )
    .await;

    assert_eq!(attempts, 1);
    assert!(answer.starts_with("Required literal stays first."));
    assert!(answer.ends_with("completeness and verification are not guaranteed."));
    assert_eq!(telemetry.provider_429_count, 1);
    assert_eq!(telemetry.provider_retry_count, 0);
    assert_eq!(telemetry.degraded_completion_count, 1);
}

#[test]
fn failed_focused_repair_returns_the_completed_initial_result_as_partial() {
    assert_eq!(
        completed_grounded_answer_after_failed_focused_follow_up(
            true,
            true,
            true,
            Some("Completed initial evidence."),
        ),
        Some("Completed initial evidence.")
    );
    assert!(
        completed_grounded_answer_after_failed_focused_follow_up(
            true,
            true,
            false,
            Some("Completed initial evidence."),
        )
        .is_none()
    );
}

#[test]
fn ready_runtime_focused_repair_passthrough_skips_model_synthesis() {
    let call = focused_grounded_answer_follow_up_call(
        "How do I configure it?",
        24,
        RuntimeGroundedRepairMetadata::from_reason(
            GroundedAnswerRepairReason::ProcedureIncomplete,
            QueryLanguage::En,
        ),
    );
    let outcome = ready_grounded_answer_passthrough_outcome(
        "Complete repaired answer.",
        "How do I configure it?",
    );

    assert_eq!(
        focused_grounded_answer_passthrough_candidate(
            true,
            std::slice::from_ref(&call),
            std::slice::from_ref(&outcome),
        )
        .as_ref()
        .map(|passthrough| passthrough.answer.as_str()),
        Some("Complete repaired answer."),
    );
    assert!(focused_grounded_answer_passthrough_candidate(false, &[call], &[outcome]).is_none());
}

#[test]
fn ready_focused_follow_up_resolves_the_gap() {
    let mut state = FocusedGroundedFollowUpState::default();
    assert!(state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::En,));
    assert!(state.take_call("How do I configure it?", 24).is_some());
    let mut outcome = synthetic_success_outcome();
    outcome.grounded_answer_ready = true;
    outcome.grounded_answer_completed = true;

    state.observe_attempt_outcome(&outcome);

    assert!(!state.requires_resolution());
    assert!(!state.is_unresolved_after_attempt());
}

#[test]
fn ui_consumes_typed_completion_gap_from_grounded_answer_result() {
    let question = "How do I configure the connector?";
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "The connector is a configurable integration component.".to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": "The connector is a configurable integration component.",
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
                "verificationState": "verified",
                "verificationWarnings": []
            }
        })),
        is_error: false,
    };

    assert_eq!(
        grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
        Some(GroundedAnswerRepairReason::ProcedureIncomplete),
    );
    assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
}

#[test]
fn ui_completion_envelope_fails_closed_when_missing_malformed_or_inconsistent() {
    let question = "Describe the sample component.";
    let missing = serde_json::json!({
        "answerBody": "Grounded text.",
        "executionDetail": {"verificationState": "verified"}
    });
    let mut malformed = with_test_completion_envelope(serde_json::json!({
        "answerBody": "Grounded text.",
        "finalAnswerReady": true,
        "finalizable": true,
        "lifecycleState": "completed",
        "executionDetail": {"verificationState": "verified"}
    }));
    malformed["repairPolicy"]["maxAdditionalGroundedAnswerCalls"] = serde_json::json!(2);
    let mut inconsistent = with_test_completion_envelope(serde_json::json!({
        "answerBody": "Grounded text.",
        "finalAnswerReady": true,
        "finalizable": true,
        "lifecycleState": "completed",
        "executionDetail": {"verificationState": "verified"}
    }));
    inconsistent["readiness"]["finalizable"] = serde_json::json!(false);

    for structured_content in [missing, malformed, inconsistent] {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Grounded text.".to_string(),
            }],
            structured_content,
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert_eq!(
            grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
            Some(GroundedAnswerRepairReason::VerificationIncomplete),
        );
    }
}

#[test]
fn typed_grounded_answer_outcome_fails_closed_on_mismatch_or_truncated_candidates() {
    let answer_body = "Choose a neutral variant.";
    let mismatched = crate::interfaces::http::mcp::McpToolResult {
        content: Vec::new(),
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": answer_body,
            "lifecycleState": "completed",
            "executionDetail": {"answerDisposition": "clarification"},
            "clarification": {"required": false, "question": null},
            "answerCandidates": []
        })),
        is_error: false,
    };
    let mut truncated_content = with_test_completion_envelope(serde_json::json!({
        "answerBody": answer_body,
        "lifecycleState": "completed",
        "clarification": {"required": true, "question": "Which neutral variant?"},
        "answerCandidates": []
    }));
    truncated_content["responseProfile"] = serde_json::json!("compact");
    truncated_content["answerCandidateSummary"] = serde_json::json!({
        "totalCount": 2,
        "returnedCount": 1,
        "truncated": true,
        "candidates": [{
            "label": "Neutral variant",
            "kind": "document",
            "provenance": {}
        }]
    });
    let truncated = crate::interfaces::http::mcp::McpToolResult {
        content: Vec::new(),
        structured_content: truncated_content,
        is_error: false,
    };

    for result in [mismatched, truncated] {
        assert!(grounded_answer_typed_outcome(GROUNDED_ANSWER_TOOL_NAME, &result).is_none());
        assert_eq!(
            grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, "Neutral question", &result),
            Some(GroundedAnswerRepairReason::VerificationIncomplete),
        );
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }
}

#[test]
fn verified_but_incomplete_procedure_cannot_finalize() {
    let question = "How do I configure the connector?";
    let result = verified_grounded_result(
        "The connector is an integration component with several configuration variants.",
        AnswerCompletionAssessment::incomplete(AnswerCompletionGapReason::Procedure, 2, 0),
    );

    assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
    assert_eq!(
        grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
        Some(GroundedAnswerRepairReason::ProcedureIncomplete),
    );
}

#[test]
fn verified_but_incomplete_troubleshooting_cannot_finalize() {
    let question = "What should I do when the operation fails with `E-17`?";
    let result = verified_grounded_result(
        "`E-17` means that the operation encountered duplicate state.",
        AnswerCompletionAssessment::incomplete(AnswerCompletionGapReason::Troubleshooting, 1, 0),
    );

    assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
    assert_eq!(
        grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
        Some(GroundedAnswerRepairReason::TroubleshootingIncomplete),
    );
}

#[test]
fn verified_but_structurally_incomplete_answer_cannot_finalize() {
    let question = "Configure Subject Alpha.";
    let result = verified_grounded_result(
        "1. Open the configuration.\n1. Open the configuration.",
        AnswerCompletionAssessment::incomplete(AnswerCompletionGapReason::AnswerStructure, 1, 0),
    );

    assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result));
    assert_eq!(
        grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
        Some(GroundedAnswerRepairReason::AnswerStructureIncomplete),
    );
}

#[test]
fn verified_but_short_latest_n_inventory_cannot_finalize() {
    let question = "What changed in the latest 4 revisions?";
    let result = verified_grounded_result(
        "1. Revision 4: improved startup.\n2. Revision 3: improved diagnostics.",
        AnswerCompletionAssessment::incomplete(AnswerCompletionGapReason::OrderedInventory, 4, 2),
    );

    assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
    assert_eq!(
        grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
        Some(GroundedAnswerRepairReason::OrderedInventory { expected: 4, observed: 2 }),
    );
}
