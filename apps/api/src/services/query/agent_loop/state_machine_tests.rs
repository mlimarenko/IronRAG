use super::*;

#[test]
fn ui_agent_tool_defs_match_mcp_answer_surface_descriptors() {
    let auth = auth_with_answer_tool_access();
    // 1:1 tool parity: for the SAME capabilities, the UI agent's visible
    // tool set must equal the external MCP answer surface's set — and the
    // single vision-gated tool (`view_document_image`) must appear on both
    // surfaces under the same condition, never on one alone.
    for agent_vision_available in [false, true] {
        let capabilities = ToolVisibilityCapabilities { agent_vision_available };
        let expected_names = tools::visible_tool_names_with_capabilities(
            &auth,
            McpToolSurface::Answer,
            capabilities,
        );
        let tool_defs =
            answer_surface_tool_defs(&auth, capabilities).expect("complete answer tool contract");

        assert_eq!(
            tool_defs.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            expected_names.iter().map(String::as_str).collect::<Vec<_>>(),
            "UI agent tool set diverged from MCP answer surface (vision={agent_vision_available})"
        );
        assert!(tool_defs.iter().any(|tool| tool.name == "grounded_answer"));
        assert!(!tool_defs.iter().any(|tool| tool.name == "create_documents"));
        assert_eq!(
            tool_defs.iter().any(|tool| tool.name == "view_document_image"),
            agent_vision_available,
            "view_document_image must track the vision capability for UI<->MCP parity"
        );

        for tool in tool_defs {
            let descriptor = tools::descriptor_for(&tool.name).expect("descriptor");
            assert_eq!(tool.description, descriptor.description);
            assert_eq!(tool.parameters, descriptor.input_schema);
        }
    }
}

#[test]
fn extracts_grounded_answer_child_runtime_execution_id_from_top_level_contract() {
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();
    let value = serde_json::json!({
        "runtimeExecutionId": first,
        "executionDetail": {
            "execution": {
                "runtimeExecutionId": first
            }
        },
        "items": [
            { "runtimeExecutionId": second },
            { "runtimeExecutionId": "not-a-uuid" }
        ]
    });

    let ids = extract_child_runtime_execution_ids(GROUNDED_ANSWER_TOOL_NAME, &value);

    assert_eq!(ids, vec![first]);
    assert_ne!(ids, vec![second]);
}

#[test]
fn extracts_grounded_answer_child_query_execution_id_from_top_level_contract() {
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();
    let value = serde_json::json!({
        "executionId": first,
        "executionDetail": {
            "execution": {
                "id": first
            }
        },
        "items": [
            { "executionId": second },
            { "executionId": "not-a-uuid" }
        ]
    });

    let ids = extract_child_query_execution_ids(GROUNDED_ANSWER_TOOL_NAME, &value);

    assert_eq!(ids, vec![first]);
    assert_ne!(ids, vec![second]);
}

#[test]
fn ignores_execution_ids_from_non_grounded_tool_results() {
    let execution_id = Uuid::now_v7();
    let runtime_execution_id = Uuid::now_v7();
    let value = serde_json::json!({
        "executionId": execution_id,
        "runtimeExecutionId": runtime_execution_id,
        "items": [
            { "executionId": Uuid::now_v7(), "runtimeExecutionId": Uuid::now_v7() }
        ]
    });

    assert!(extract_child_query_execution_ids("list_documents", &value).is_empty());
    assert!(extract_child_runtime_execution_ids("list_documents", &value).is_empty());
}

#[test]
fn final_iteration_disables_tools_after_prior_tool_calls() {
    let started = Instant::now();
    let names = BTreeSet::new();

    assert!(force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 5,
        iteration_cap: 5,
        total_tool_call_count: 1,
        successful_tool_call_count: 1,
        verified_grounded_answer_count: 0,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: None,
    }));
    assert!(force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 1,
        iteration_cap: 1,
        total_tool_call_count: 1,
        successful_tool_call_count: 1,
        verified_grounded_answer_count: 0,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: None,
    }));
    assert!(!force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 4,
        iteration_cap: 5,
        total_tool_call_count: 1,
        successful_tool_call_count: 1,
        verified_grounded_answer_count: 0,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: None,
    }));
    assert!(!force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 5,
        iteration_cap: 5,
        total_tool_call_count: 0,
        successful_tool_call_count: 0,
        verified_grounded_answer_count: 0,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: None,
    }));
}

#[test]
fn forced_final_request_messages_add_answer_only_reminder() {
    let messages = vec![ChatMessage::system("policy"), ChatMessage::user("Q?")];

    let normal_request = final_answer_request_messages(&messages, false);
    let forced_request = final_answer_request_messages(&messages, true);

    assert_eq!(normal_request.len(), messages.len());
    assert_eq!(forced_request.len(), messages.len() + 1);
    assert_eq!(forced_request.last().expect("reminder").role, "system");
    assert!(
        forced_request
            .last()
            .and_then(|message| message.content.as_deref())
            .expect("reminder content")
            .contains("No more MCP tool calls")
    );
}

#[test]
fn forced_final_retry_messages_close_illegal_tool_call() {
    let request_messages = vec![ChatMessage::system("policy"), ChatMessage::user("Q?")];
    let tool_calls = vec![ChatToolCall {
        id: "call-1".to_string(),
        name: READ_DOCUMENT_TOOL_NAME.to_string(),
        arguments_json: "{}".to_string(),
    }];

    let retry_messages =
        final_answer_retry_messages(&request_messages, Some("reasoning".to_string()), &tool_calls);

    assert_eq!(retry_messages.len(), request_messages.len() + 3);
    let assistant = &retry_messages[request_messages.len()];
    assert_eq!(assistant.role, "assistant");
    assert_eq!(assistant.reasoning_content.as_deref(), Some("reasoning"));
    assert_eq!(assistant.tool_calls.len(), 1);
    let tool_result = &retry_messages[request_messages.len() + 1];
    assert_eq!(tool_result.role, "tool");
    assert_eq!(tool_result.tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(tool_result.name.as_deref(), Some(READ_DOCUMENT_TOOL_NAME));
    assert!(tool_result.content.as_deref().expect("tool result content").contains("not executed"));
    assert_eq!(retry_messages.last().expect("reminder").role, "system");
}

#[test]
fn soft_deadline_keeps_tools_without_verified_grounded_answer() {
    let started = Instant::now() - Duration::from_secs(40);
    let names =
        BTreeSet::from([SEARCH_DOCUMENTS_TOOL_NAME.to_string(), "search_entities".to_string()]);

    assert!(!force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 3,
        iteration_cap: 5,
        total_tool_call_count: 4,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
        verified_grounded_answer_count: 0,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
}

#[test]
fn soft_deadline_keeps_collecting_when_tool_evidence_has_one_category() {
    let started = Instant::now() - Duration::from_secs(40);
    let names = BTreeSet::from([SEARCH_DOCUMENTS_TOOL_NAME.to_string()]);

    assert!(!force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 3,
        iteration_cap: 5,
        total_tool_call_count: 4,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
        verified_grounded_answer_count: 0,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
}

#[test]
fn soft_deadline_waits_for_enough_successful_tools() {
    let started = Instant::now() - Duration::from_secs(40);
    let names = BTreeSet::new();

    assert!(!force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 3,
        iteration_cap: 5,
        total_tool_call_count: 3,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS - 1,
        verified_grounded_answer_count: 0,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
}

#[test]
fn grounded_answer_success_alone_waits_until_soft_deadline() {
    let before_deadline = Instant::now();
    let after_deadline = Instant::now() - Duration::from_secs(40);
    let names = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

    assert!(!force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 3,
        iteration_cap: 5,
        total_tool_call_count: 5,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
        verified_grounded_answer_count: 1,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started: before_deadline,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
    assert!(force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 3,
        iteration_cap: 5,
        total_tool_call_count: 5,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
        verified_grounded_answer_count: 1,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started: after_deadline,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
}

#[test]
fn soft_deadline_does_not_force_final_when_grounded_follow_up_is_required() {
    let after_deadline = Instant::now() - Duration::from_secs(40);
    let names = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

    assert!(!force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 3,
        iteration_cap: 5,
        total_tool_call_count: 5,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
        verified_grounded_answer_count: 1,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: true,
        started: after_deadline,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
    assert!(force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 5,
        iteration_cap: 5,
        total_tool_call_count: 5,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
        verified_grounded_answer_count: 1,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: true,
        started: after_deadline,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
}

#[test]
fn grounded_answer_success_keeps_tool_surface_available_for_agent_judgment() {
    let tool_defs = [
        GROUNDED_ANSWER_TOOL_NAME,
        SEARCH_DOCUMENTS_TOOL_NAME,
        READ_DOCUMENT_TOOL_NAME,
        "search_entities",
    ]
    .into_iter()
    .map(|name| ChatToolDef {
        name: name.to_string(),
        description: String::new(),
        parameters: serde_json::json!({}),
    })
    .collect::<Vec<_>>();
    let successful = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

    let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful, false);

    assert_eq!(
        next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        vec![
            GROUNDED_ANSWER_TOOL_NAME,
            SEARCH_DOCUMENTS_TOOL_NAME,
            READ_DOCUMENT_TOOL_NAME,
            "search_entities"
        ]
    );
}

#[test]
fn composite_doc_graph_evidence_keeps_tools_without_verified_grounded_answer() {
    let started = Instant::now();
    let names = BTreeSet::from([
        SEARCH_DOCUMENTS_TOOL_NAME.to_string(),
        "search_entities".to_string(),
        READ_DOCUMENT_TOOL_NAME.to_string(),
    ]);

    assert!(!force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 3,
        iteration_cap: 5,
        total_tool_call_count: 4,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
        verified_grounded_answer_count: 0,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
}

#[test]
fn verified_grounded_composite_evidence_can_disable_tools_before_soft_deadline() {
    let started = Instant::now();
    let names = BTreeSet::from([
        GROUNDED_ANSWER_TOOL_NAME.to_string(),
        SEARCH_DOCUMENTS_TOOL_NAME.to_string(),
        READ_DOCUMENT_TOOL_NAME.to_string(),
    ]);

    assert!(force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 3,
        iteration_cap: 5,
        total_tool_call_count: 4,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
        verified_grounded_answer_count: 1,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
}

#[test]
fn soft_deadline_keeps_tools_before_deadline() {
    let started = Instant::now();
    let names = BTreeSet::from([SEARCH_DOCUMENTS_TOOL_NAME.to_string()]);

    assert!(!force_final_answer_iteration(FinalAnswerForcingInput {
        iteration: 3,
        iteration_cap: 5,
        total_tool_call_count: 4,
        successful_tool_call_count: SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
        verified_grounded_answer_count: 0,
        successful_tool_names: &names,
        incomplete_grounded_answer_needs_follow_up: false,
        started,
        soft_final_answer_deadline: Some(Duration::from_secs(35)),
    }));
}

#[test]
fn final_answer_requires_at_least_one_tool_result() {
    let tool_defs = [
        "list_workspaces",
        "list_libraries",
        "grounded_answer",
        "search_documents",
        "search_entities",
        "read_document",
    ]
    .into_iter()
    .map(|name| ChatToolDef {
        name: name.to_string(),
        description: String::new(),
        parameters: serde_json::json!({}),
    })
    .collect::<Vec<_>>();

    assert!(should_require_tool_call_before_final(false, &tool_defs, &BTreeSet::new(), false));

    let simple_evidence = BTreeSet::from(["search_documents".to_string()]);
    assert!(!should_require_tool_call_before_final(false, &tool_defs, &simple_evidence, false,));

    let composite_evidence =
        BTreeSet::from(["search_documents".to_string(), "search_entities".to_string()]);
    assert!(!should_require_tool_call_before_final(false, &tool_defs, &composite_evidence, false,));

    let complete_composite = BTreeSet::from([
        "search_documents".to_string(),
        "search_entities".to_string(),
        "read_document".to_string(),
    ]);
    assert!(!should_require_tool_call_before_final(false, &tool_defs, &complete_composite, false,));
    assert!(!should_require_tool_call_before_final(true, &tool_defs, &BTreeSet::new(), true,));
    assert!(!should_require_tool_call_before_final(false, &[], &BTreeSet::new(), true,));

    let answer_only = [ChatToolDef {
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        description: String::new(),
        parameters: serde_json::json!({}),
    }];
    assert!(should_require_tool_call_before_final(false, &answer_only, &BTreeSet::new(), false));
    assert!(!should_require_tool_call_before_final(
        false,
        &answer_only,
        &BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]),
        false,
    ));
}

#[test]
fn required_grounded_answer_fallback_only_runs_before_forced_final() {
    let tool_defs = [ChatToolDef {
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        description: String::new(),
        parameters: serde_json::json!({}),
    }];

    assert!(should_inject_required_grounded_answer_tool_call(true, true, false, &tool_defs,));
    assert!(!should_inject_required_grounded_answer_tool_call(false, true, false, &tool_defs,));
    assert!(!should_inject_required_grounded_answer_tool_call(true, false, false, &tool_defs,));
    assert!(!should_inject_required_grounded_answer_tool_call(true, true, true, &tool_defs,));
    assert!(!should_inject_required_grounded_answer_tool_call(true, true, false, &[]));
}

#[test]
fn required_grounded_answer_fallback_preserves_query_and_top_k() {
    let call = required_grounded_answer_tool_call("Q?", 0);
    let arguments = serde_json::from_str::<serde_json::Value>(&call.arguments_json).expect("json");

    assert_eq!(call.name, GROUNDED_ANSWER_TOOL_NAME);
    assert_eq!(arguments["query"], "Q?");
    assert_eq!(arguments["topK"], 1);
    assert_eq!(arguments["responseProfile"], "compact");
    assert_eq!(
        arguments["maxReferences"],
        crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT
    );
}

#[test]
fn final_answer_requires_follow_up_after_incomplete_grounded_answer() {
    let tool_defs =
        [GROUNDED_ANSWER_TOOL_NAME, SEARCH_DOCUMENTS_TOOL_NAME, READ_DOCUMENT_TOOL_NAME]
            .into_iter()
            .map(|name| ChatToolDef {
                name: name.to_string(),
                description: String::new(),
                parameters: serde_json::json!({}),
            })
            .collect::<Vec<_>>();
    let grounded = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

    assert!(should_require_tool_call_before_final(false, &tool_defs, &grounded, true));
}

#[test]
fn complete_grounded_answer_allows_final_synthesis() {
    let tool_defs = [GROUNDED_ANSWER_TOOL_NAME]
        .into_iter()
        .map(|name| ChatToolDef {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect::<Vec<_>>();
    let grounded = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

    assert!(!should_require_tool_call_before_final(false, &tool_defs, &grounded, false));
}

#[test]
fn typed_clarification_is_terminal_exact_passthrough_without_retry() {
    let question = "How do I configure the connector?";
    let answer_body = "Choose one of the matching variants.";
    let result = crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: answer_body.to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "queryLanguage": "en",
            "answerBody": answer_body,
            "finalAnswerReady": false,
            "finalizable": false,
            "lifecycleState": "completed",
            "clarification": {
                "required": true,
                "question": "Which variant?"
            },
            "answerCandidates": [
                {"label": "Variant Alpha", "kind": "document", "provenance": {}},
                {"label": "Variant Beta", "kind": "document", "provenance": {}}
            ],
            "executionDetail": {
                "verificationState": "not_run",
                "verificationWarnings": [{
                    "code": "clarification_not_answer",
                    "message": "This is a clarification, not an answer."
                }]
            }
        })),
        is_error: false,
    };
    let call = ChatToolCall {
        id: "call-grounded".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({"query": question}).to_string(),
    };
    let repair_reason = grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result);
    let mut state = FocusedGroundedFollowUpState::default();
    if let Some(reason) = repair_reason {
        state.schedule(reason, grounded_answer_query_language(GROUNDED_ANSWER_TOOL_NAME, &result));
    }
    let grounded_answer_outcome = grounded_answer_typed_outcome(GROUNDED_ANSWER_TOOL_NAME, &result);
    let grounded_answer_clarification_required =
        grounded_answer_outcome.as_ref().is_some_and(|outcome| {
            matches!(outcome.disposition, QueryAnswerDisposition::Clarification)
        });
    let outcome = ToolExecutionOutcome {
        arguments_json: Some(serde_json::json!({"query": question}).to_string()),
        requested_arguments_json: None,
        message_content: String::new(),
        result_text: Some(answer_body.to_string()),
        result_json: Some(debug_tool_result_json(&result)),
        grounding_text: None,
        grounded_answer_body: grounded_answer_verbatim_body(GROUNDED_ANSWER_TOOL_NAME, &result),
        grounded_answer_outcome,
        grounded_answer_ready: false,
        grounded_answer_completed: grounded_answer_completed(GROUNDED_ANSWER_TOOL_NAME, &result),
        grounded_answer_needs_follow_up: grounded_answer_needs_follow_up(
            GROUNDED_ANSWER_TOOL_NAME,
            &result,
        ),
        grounded_answer_repair_reason: repair_reason,
        grounded_answer_language: grounded_answer_query_language(
            GROUNDED_ANSWER_TOOL_NAME,
            &result,
        ),
        grounded_answer_clarification_required,
        is_error: false,
        is_replay: false,
        duration_ms: 5,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    };

    assert!(repair_reason.is_none());
    assert!(!state.requires_resolution());
    assert!(state.take_call(question, 24).is_none());
    assert!(!outcome.grounded_answer_needs_follow_up);
    let terminal = terminal_grounded_answer_nonfactual_candidate(
        std::slice::from_ref(&call),
        std::slice::from_ref(&outcome),
        question,
    )
    .expect("typed clarification should terminate the initial grounded-answer call");
    let metadata = agent_loop_metadata(6, Duration::from_secs(60), terminal.stop_reason, 1);

    assert_eq!(terminal.answer, answer_body);
    assert_eq!(terminal.grounded_answer_outcome.disposition, QueryAnswerDisposition::Clarification,);
    assert_eq!(terminal.grounded_answer_outcome.clarification.answer_candidates.len(), 2);
    assert_eq!(metadata.stopped_reason, AgentStopReason::FinalAnswer);
    assert_eq!(metadata.tool_call_count, 1);
    assert_eq!(
        outcome
            .result_json
            .as_ref()
            .and_then(|json| json.pointer("/structuredContent/clarification")),
        Some(&serde_json::json!({
            "required": true,
            "question": "Which variant?",
        })),
    );
}
