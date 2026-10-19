use super::*;

pub(super) fn with_test_completion_envelope(mut structured: Value) -> Value {
    let candidate_ready =
        structured.get("finalAnswerReady").and_then(Value::as_bool).unwrap_or(false);
    let answer_text = structured.get("answerBody").and_then(Value::as_str).unwrap_or_default();
    let lifecycle_state =
        structured.get("lifecycleState").and_then(Value::as_str).unwrap_or("completed");
    let clarification_required =
        structured.pointer("/clarification/required").and_then(Value::as_bool).unwrap_or(false);
    let answer_disposition = structured
        .pointer("/executionDetail/answerDisposition")
        .cloned()
        .map(|value| {
            serde_json::from_value(value)
                .expect("test answer disposition must use the contract vocabulary")
        })
        .unwrap_or_else(|| {
            if clarification_required {
                ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification
            } else if candidate_ready {
                ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
            } else {
                ironrag_contracts::assistant::AssistantAnswerDisposition::NonTerminal
            }
        });
    let completion = structured
        .get("completion")
        .cloned()
        .map(|value| {
            serde_json::from_value(value)
                .expect("test completion assessment must match the typed contract")
        })
        .filter(AnswerCompletionAssessment::is_consistent)
        .unwrap_or_else(AnswerCompletionAssessment::complete);
    let envelope = GroundedAnswerCompletionEnvelope::new(
        answer_disposition,
        answer_text,
        completion,
        lifecycle_state,
        None,
    );
    let Value::Object(envelope_fields) = serde_json::json!(envelope) else {
        return structured;
    };
    if let Some(fields) = structured.as_object_mut() {
        fields.extend(envelope_fields);
        fields
            .entry("responseProfile".to_string())
            .or_insert_with(|| Value::String("full".to_string()));
        fields.entry("clarification".to_string()).or_insert_with(|| {
            serde_json::json!({
                "required": clarification_required,
                "question": Value::Null,
            })
        });
        fields.entry("answerCandidates".to_string()).or_insert_with(|| Value::Array(Vec::new()));
    }
    structured
}

pub(super) fn auth_with_answer_tool_access() -> AuthContext {
    AuthContext {
        token_id: Uuid::nil(),
        principal_id: Uuid::nil(),
        parent_principal_id: None,
        workspace_id: None,
        token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
        scopes: Vec::new(),
        grants: vec![
            AuthGrant {
                id: Uuid::from_u128(1),
                resource_kind: "library".to_string(),
                resource_id: Uuid::from_u128(11),
                permission_kind: POLICY_QUERY_RUN[0].to_string(),
                workspace_id: Some(Uuid::from_u128(101)),
                library_id: Some(Uuid::from_u128(11)),
                document_id: None,
            },
            AuthGrant {
                id: Uuid::from_u128(2),
                resource_kind: "library".to_string(),
                resource_id: Uuid::from_u128(11),
                permission_kind: POLICY_MCP_MEMORY_READ[0].to_string(),
                workspace_id: Some(Uuid::from_u128(101)),
                library_id: Some(Uuid::from_u128(11)),
                document_id: None,
            },
            AuthGrant {
                id: Uuid::from_u128(3),
                resource_kind: "library".to_string(),
                resource_id: Uuid::from_u128(11),
                permission_kind: POLICY_RUNTIME_READ[0].to_string(),
                workspace_id: Some(Uuid::from_u128(101)),
                library_id: Some(Uuid::from_u128(11)),
                document_id: None,
            },
            AuthGrant {
                id: Uuid::from_u128(4),
                resource_kind: "library".to_string(),
                resource_id: Uuid::from_u128(11),
                permission_kind: POLICY_LIBRARY_READ[0].to_string(),
                workspace_id: Some(Uuid::from_u128(101)),
                library_id: Some(Uuid::from_u128(11)),
                document_id: None,
            },
        ],
        workspace_memberships: Vec::new(),
        visible_workspace_ids: BTreeSet::new(),
        is_system_admin: false,
        system_role: None,
    }
}
pub(super) fn verified_grounded_result(
    answer_body: &str,
    completion: AnswerCompletionAssessment,
) -> crate::interfaces::http::mcp::McpToolResult {
    crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: answer_body.to_string(),
        }],
        structured_content: with_test_completion_envelope(serde_json::json!({
            "answerBody": answer_body,
            "finalAnswerReady": true,
            "finalizable": true,
            "lifecycleState": "completed",
            "completion": completion,
            "executionDetail": {
                "verificationState": "verified",
                "verificationWarnings": []
            }
        })),
        is_error: false,
    }
}
pub(super) fn grounded_answer_ledger_outcome(mut result_json: Value) -> ToolExecutionOutcome {
    if let Some(structured_content) = result_json.get_mut("structuredContent") {
        *structured_content = with_test_completion_envelope(std::mem::take(structured_content));
    }
    ToolExecutionOutcome {
        arguments_json: None,
        requested_arguments_json: None,
        message_content: String::new(),
        result_text: result_json
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        result_json: Some(result_json),
        grounding_text: None,
        grounded_answer_body: None,
        grounded_answer_outcome: None,
        grounded_answer_ready: false,
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
    }
}

pub(super) fn ready_grounded_answer_passthrough_outcome(
    answer_body: &str,
    executed_query: &str,
) -> ToolExecutionOutcome {
    let structured_content = with_test_completion_envelope(serde_json::json!({
        "answerBody": answer_body,
        "finalAnswerReady": true,
        "finalizable": true,
        "completion": {"complete": true},
        "lifecycleState": "completed",
    }));
    ToolExecutionOutcome {
        arguments_json: Some(serde_json::json!({"query": executed_query}).to_string()),
        requested_arguments_json: None,
        message_content: String::new(),
        result_text: Some("human text must not replace structured answerBody".to_string()),
        result_json: Some(serde_json::json!({
            "structuredContent": structured_content,
            "isError": false
        })),
        grounding_text: Some("verifier-grade evidence".to_string()),
        grounded_answer_body: Some(answer_body.to_string()),
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
        duration_ms: 5,
        child_query_execution_ids: vec![Uuid::from_u128(101)],
        child_runtime_execution_ids: vec![Uuid::from_u128(102)],
    }
}
pub(super) fn synthetic_success_outcome() -> ToolExecutionOutcome {
    ToolExecutionOutcome {
        arguments_json: Some("{}".to_string()),
        requested_arguments_json: None,
        message_content: "{}".to_string(),
        result_text: Some("ok".to_string()),
        result_json: None,
        grounding_text: None,
        grounded_answer_body: None,
        grounded_answer_outcome: None,
        grounded_answer_ready: false,
        grounded_answer_completed: false,
        grounded_answer_needs_follow_up: false,
        grounded_answer_repair_reason: None,
        grounded_answer_language: QueryLanguage::Auto,
        grounded_answer_clarification_required: false,
        is_error: false,
        is_replay: false,
        duration_ms: 0,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    }
}
pub(super) fn tool_result_json(error_kind: &str, retryable: bool) -> Value {
    debug_tool_result_json(&crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "document projection is still updating; retry shortly".to_string(),
        }],
        structured_content: serde_json::json!({
            "errorKind": error_kind,
            "message": "document projection is still updating; retry shortly",
            "retryable": retryable,
        }),
        is_error: true,
    })
}
