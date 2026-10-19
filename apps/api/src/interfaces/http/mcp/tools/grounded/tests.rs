use chrono::Utc;
use ironrag_contracts::assistant::{
    AssistantAnswerDisposition, AssistantChunkReference, AssistantContentSourceAccess,
    AssistantEntityReference, AssistantExecution, AssistantExecutionDetail, AssistantPolicySummary,
    AssistantPreparedSegmentReference, AssistantRelationReference, AssistantRuntimeSummary,
    AssistantTechnicalFactReference, AssistantVerificationState,
};
use uuid::Uuid;

use crate::domains::query_ir::{
    QueryAct, QueryIR, QueryLanguage, QueryScope, SourceSliceDirection, SourceSliceFilter,
    SourceSliceSpec,
};

use super::*;

#[test]
fn prepared_grounded_answer_preserves_normalized_history_and_response_policy() {
    let request = prepare_grounded_answer_request(&json!({
        "library": " workspace/library ",
        "query": " exact question ",
        "conversationTurns": [
            {"role": "user", "content": " previous question "},
            {"role": "assistant", "content": " previous answer "}
        ],
        "topK": 4,
        "maxReferences": 3
    }))
    .expect("valid contextual request");
    assert_eq!(request.library.as_deref(), Some("workspace/library"));
    assert_eq!(request.query, "exact question");
    assert_eq!(request.top_k, DEFAULT_TOP_K);
    assert!(!request.include_debug);
    assert_eq!(request.response_profile, McpGroundedAnswerResponseProfile::Compact);
    assert_eq!(request.compact_reference_limit, Some(3));
    assert_eq!(request.external_prior_turns.len(), 2);
    assert_eq!(request.external_prior_turns[0].turn_kind, QueryTurnKind::User);
    assert_eq!(request.external_prior_turns[0].content_text, "previous question");
    assert_eq!(request.external_prior_turns[1].turn_kind, QueryTurnKind::Assistant);
    assert_eq!(request.external_prior_turns[1].content_text, "previous answer");
}

#[test]
fn prepared_grounded_answer_keeps_debug_and_fresh_turn_defaults_distinct() {
    let request = prepare_grounded_answer_request(&json!({
        "query": "question", "topK": 4, "includeDebug": true
    }))
    .expect("valid debug request");
    assert!(request.library.is_none());
    assert!(request.external_prior_turns.is_empty());
    assert_eq!(request.top_k, 4);
    assert!(request.include_debug);
    assert_eq!(request.response_profile, McpGroundedAnswerResponseProfile::Full);
    assert_eq!(request.compact_reference_limit, None);
    assert!(
        prepare_grounded_answer_request(&json!({
            "query": "question", "includeDebug": true, "responseProfile": "compact"
        }))
        .is_err()
    );
    assert!(
        prepare_grounded_answer_request(&json!({
            "query": "question", "responseProfile": "full", "maxReferences": 3
        }))
        .is_err()
    );
}

#[test]
fn grounded_answer_carries_the_explicit_typed_query_language() {
    let mut result = McpToolResult {
        content: Vec::new(),
        structured_content: json!({"answerBody": "Synthetic answer."}),
        is_error: false,
    };

    attach_grounded_answer_query_language(&mut result, QueryLanguage::Ru);

    assert_eq!(result.structured_content["queryLanguage"], json!("ru"));
}

#[test]
fn grounded_answer_default_top_k_matches_ui_query_turn_default() {
    let library_ref = "alpha-workspace/adapter-library";
    let query = "Which endpoint does the demo adapter call for inventory sync?";

    let mcp_top_k = resolve_grounded_answer_top_k(None, false);
    let ui_top_k = crate::interfaces::http::query::resolve_query_turn_top_k(None);

    assert_eq!(mcp_top_k, ui_top_k, "top_k drift for library {library_ref} and query {query}");
    assert_eq!(mcp_top_k, DEFAULT_TOP_K);
    assert!(mcp_top_k >= 24);
}

#[test]
fn grounded_answer_explicit_top_k_matches_ui_query_turn() {
    assert_eq!(
        resolve_grounded_answer_top_k(None, false),
        crate::interfaces::http::query::resolve_query_turn_top_k(None)
    );
    assert_eq!(
        resolve_grounded_answer_top_k(Some(6), false),
        crate::interfaces::http::query::resolve_query_turn_top_k(Some(6))
    );
}

#[test]
fn omitted_response_profile_is_compact_for_ui_and_external_mcp() {
    for surface in [
        crate::domains::agent_runtime::RuntimeSurfaceKind::Ui,
        crate::domains::agent_runtime::RuntimeSurfaceKind::Mcp,
    ] {
        let routing = grounded_answer_conversation_routing(surface);
        assert_eq!(routing.runtime_surface, surface);
        assert_eq!(
            resolve_grounded_answer_response_profile(None, None),
            McpGroundedAnswerResponseProfile::Compact,
            "omitted profile drifted for {surface:?}",
        );
    }
    assert_eq!(
        resolve_grounded_answer_response_profile(Some(true), None),
        McpGroundedAnswerResponseProfile::Full,
    );
    assert_eq!(
        resolve_grounded_answer_response_profile(
            None,
            Some(McpGroundedAnswerResponseProfile::Full),
        ),
        McpGroundedAnswerResponseProfile::Full,
    );
}

#[test]
fn grounded_answer_contextual_top_k_floor_matches_ui_agent_tool_default() {
    // A contextual follow-up floors retrieval breadth to the configured
    // default so it never retrieves more narrowly than a fresh turn.
    assert_eq!(resolve_grounded_answer_top_k(Some(4), true), DEFAULT_TOP_K);
    assert_eq!(resolve_grounded_answer_top_k(Some(4), false), 4);
}

#[test]
fn grounded_answer_descriptor_is_bounded_and_preserves_agent_policy_signals() {
    let descriptor = descriptor("grounded_answer").expect("descriptor");
    assert!(
        descriptor.description.len()
            <= crate::services::mcp::agent_policy::GROUNDED_ANSWER_DESCRIPTION_MAX_BYTES
    );
    for signal in [
        crate::services::mcp::agent_policy::AGENT_POLICY_VERSION,
        "exact current user question",
        "built-in UI dispatches it",
        "responseProfile=compact",
        "maxReferences<=8",
        "finalAnswerReady=true",
        "one exact-query repair",
        "repairPolicy",
        "maxAdditionalGroundedAnswerCalls",
        "clarification.required=true",
        "mustPreserveSpans",
    ] {
        assert!(descriptor.description.contains(signal), "missing descriptor signal: {signal}");
    }
}

#[test]
fn grounded_answer_descriptor_advertises_compact_agent_profile() {
    let descriptor = descriptor("grounded_answer").expect("descriptor");
    let properties = &descriptor.input_schema["properties"];

    assert_eq!(properties["responseProfile"]["enum"], json!(["full", "compact"]));
    assert_eq!(properties["responseProfile"]["default"], json!("compact"));
    assert_eq!(properties["maxReferences"]["minimum"], json!(1));
    assert_eq!(
        properties["maxReferences"]["maximum"],
        json!(crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT)
    );
}

#[test]
fn grounded_answer_descriptor_allows_only_unambiguous_library_inference() {
    let descriptor = descriptor("grounded_answer").expect("descriptor");
    let schema = &descriptor.input_schema;
    let library_description =
        schema["properties"]["library"]["description"].as_str().expect("library description");

    assert_eq!(schema["required"], json!(["query"]));
    assert!(library_description.contains("exactly one"));
    assert!(library_description.contains("query_run"));
    assert!(library_description.contains("zero or multiple"));
}

#[test]
fn grounded_answer_args_accept_omitted_library_and_preserve_explicit_library() {
    let mut inferred = serde_json::from_value::<GroundedAnswerArgs>(json!({
        "query": "question"
    }))
    .expect("omitted library is valid before authorization-aware inference");
    validate_and_normalize_grounded_answer_args(&mut inferred).expect("validate omitted library");
    assert_eq!(inferred.library.as_deref(), None);

    let mut explicit = serde_json::from_value::<GroundedAnswerArgs>(json!({
        "library": "  workspace/library  ",
        "query": "question"
    }))
    .expect("parse explicit library");
    validate_and_normalize_grounded_answer_args(&mut explicit).expect("validate explicit library");
    assert_eq!(explicit.library.as_deref(), Some("workspace/library"));
}

#[test]
fn conversation_title_preserves_actual_surface() {
    assert_eq!(conversation_title("mcp", "  Lookup adapters  "), "[MCP] Lookup adapters");
    assert_eq!(conversation_title("ui", ""), "[UI] grounded_answer");
}

#[test]
fn conversation_title_preserves_exact_domain_limit_for_all_runtime_surfaces() {
    use crate::{
        domains::agent_runtime::RuntimeSurfaceKind,
        services::query::service::QUERY_CONVERSATION_TITLE_LIMIT,
    };

    for surface in [
        RuntimeSurfaceKind::Ui,
        RuntimeSurfaceKind::Rest,
        RuntimeSurfaceKind::Mcp,
        RuntimeSurfaceKind::Worker,
        RuntimeSurfaceKind::Internal,
    ] {
        let prefix = format!("[{}] ", surface.as_str().to_ascii_uppercase());
        let query = "x".repeat(QUERY_CONVERSATION_TITLE_LIMIT - prefix.chars().count());
        let title = conversation_title(surface.as_str(), &query);

        assert_eq!(title.chars().count(), QUERY_CONVERSATION_TITLE_LIMIT);
        assert_eq!(title, format!("{prefix}{query}"));
    }
}

#[test]
fn conversation_title_truncates_long_unicode_within_total_domain_limit() {
    use crate::{
        domains::agent_runtime::RuntimeSurfaceKind,
        services::query::service::QUERY_CONVERSATION_TITLE_LIMIT,
    };

    let query = "🧩".repeat(QUERY_CONVERSATION_TITLE_LIMIT * 2);
    for surface in [
        RuntimeSurfaceKind::Ui,
        RuntimeSurfaceKind::Rest,
        RuntimeSurfaceKind::Mcp,
        RuntimeSurfaceKind::Worker,
        RuntimeSurfaceKind::Internal,
    ] {
        let prefix = format!("[{}] ", surface.as_str().to_ascii_uppercase());
        let visible_query = "🧩".repeat(QUERY_CONVERSATION_TITLE_LIMIT - prefix.chars().count());
        let title = conversation_title(surface.as_str(), &query);

        assert_eq!(title.chars().count(), QUERY_CONVERSATION_TITLE_LIMIT);
        assert_eq!(title, format!("{prefix}{visible_query}"));
    }
}

#[test]
fn ui_tool_context_keeps_runtime_surface_but_uses_transient_retention_storage() {
    use crate::domains::agent_runtime::RuntimeSurfaceKind;

    for runtime_surface in [RuntimeSurfaceKind::Ui, RuntimeSurfaceKind::Mcp] {
        let routing = grounded_answer_conversation_routing(runtime_surface);

        assert_eq!(routing.runtime_surface, runtime_surface);
        assert_eq!(routing.storage_surface, RuntimeSurfaceKind::Mcp.as_str());
        assert_ne!(routing.storage_surface, RuntimeSurfaceKind::Ui.as_str());
    }
}

#[test]
fn grounded_answer_execution_error_does_not_expose_internal_context() {
    let private_context = "private user question and database failure chain";
    let result = tool_error_result(sanitize_grounded_answer_execution_error(
        ApiError::InternalMessage(private_context.to_string()),
    ));
    let serialized = serde_json::to_string(&result).expect("serialize MCP error result");

    assert!(result.is_error);
    assert_eq!(result.structured_content["errorKind"], json!("query_execution_failed"));
    assert_eq!(result.structured_content["retryable"], json!(false));
    assert!(result.structured_content.get("retryAfterMs").is_none());
    assert!(result.structured_content.get("repairHint").is_none());
    assert!(!serialized.contains(private_context));
    assert!(serialized.contains("grounded answer execution failed"));
}

#[test]
fn grounded_answer_execution_errors_preserve_safe_retryable_failure_kinds() {
    let private_context = "private question, credential label, and upstream chain";
    let cases = [
        (
            ApiError::service_unavailable(
                format!("document projection updating: {private_context}"),
                "document_projection_converging",
            ),
            "document_projection_converging",
            "document projection is still updating; retry shortly",
        ),
        (
            ApiError::ProviderFailure(format!("provider request failed: {private_context}")),
            "query_provider_unavailable",
            "query provider is temporarily unavailable; retry shortly",
        ),
        (
            ApiError::service_unavailable(private_context, "query_binding_unavailable"),
            "query_binding_unavailable",
            "query AI binding is unavailable; retry after configuration is restored",
        ),
        (
            ApiError::service_unavailable(private_context, "query_retrieval_unavailable"),
            "query_retrieval_unavailable",
            "query retrieval is temporarily unavailable; retry shortly",
        ),
        (
            ApiError::service_unavailable(private_context, "query_dependency_unavailable"),
            "query_retrieval_unavailable",
            "query retrieval is temporarily unavailable; retry shortly",
        ),
    ];

    for (error, expected_kind, expected_message) in cases {
        let result = tool_error_result(sanitize_grounded_answer_execution_error(error));
        let serialized = serde_json::to_string(&result).expect("serialize MCP error result");

        assert!(result.is_error);
        assert_eq!(result.structured_content["errorKind"], json!(expected_kind));
        assert_eq!(result.structured_content["retryable"], json!(true));
        if expected_kind == "query_binding_unavailable" {
            assert_eq!(result.structured_content["repairHint"], json!("restore_query_binding"));
            assert!(result.structured_content.get("retryAfterMs").is_none());
        } else {
            assert_eq!(result.structured_content["repairHint"], json!("retry_same_request"));
            let retry_after_ms = result.structured_content["retryAfterMs"]
                .as_u64()
                .expect("retryable transient error must carry bounded retryAfterMs");
            assert!((1..=5_000).contains(&retry_after_ms));
        }
        assert!(serialized.contains(expected_message), "{serialized}");
        assert!(!serialized.contains(private_context), "{serialized}");
    }
}

#[test]
fn grounded_answer_execution_error_ignores_misleading_untyped_text() {
    for error in [
        ApiError::Conflict("active query compile binding is not configured: opaque".to_string()),
        ApiError::InternalMessage("all chunk retrieval lanes failed: opaque".to_string()),
    ] {
        let result = tool_error_result(sanitize_grounded_answer_execution_error(error));

        assert_eq!(result.structured_content["errorKind"], json!("query_execution_failed"));
        assert_eq!(result.structured_content["retryable"], json!(false));
    }
}

#[test]
fn structured_content_embeds_expected_assistant_execution_detail() {
    let ids = ExecutionDetailIds::sequential(1);
    let detail = sample_execution_detail(ids);

    let answer = "Use `/etc/alpha.ini` with `alphaKey = true`.";
    let structured =
        crate::interfaces::http::mcp::grounded_answer_contract_payload(answer, &detail);
    let structured_content = &structured["structuredContent"];
    let execution_detail = &structured_content["executionDetail"];

    assert_eq!(structured["isError"], json!(false));
    assert_eq!(structured_content.get("citations"), None);
    assert_eq!(structured_content["answerBody"], json!(answer));
    assert_eq!(structured_content["finalAnswerReady"], json!(true));
    assert_eq!(structured_content["finalizable"], json!(true));
    assert_eq!(structured_content["repairPolicy"]["required"], json!(false));
    assert_eq!(structured_content["repairPolicy"]["maxAdditionalGroundedAnswerCalls"], json!(0));
    assert_eq!(
        structured_content["mustPreserveSpans"],
        json!([
            "/etc/alpha.ini",
            "alphaKey = true",
            "Synthetic API calls endpoint",
            "Synthetic API",
            "Synthetic API node",
            "Synthetic contract"
        ])
    );
    assert_execution_detail_reference_ids(execution_detail, &ids);
}

fn assert_execution_detail_reference_ids(
    execution_detail: &serde_json::Value,
    ids: &ExecutionDetailIds,
) {
    assert_eq!(execution_detail["chunkReferences"][0]["executionId"], json!(ids.execution_id));
    assert_eq!(execution_detail["chunkReferences"][0]["chunkId"], json!(ids.chunk_id));
    assert_eq!(
        execution_detail["preparedSegmentReferences"][0]["executionId"],
        json!(ids.execution_id)
    );
    assert_eq!(
        execution_detail["preparedSegmentReferences"][0]["segmentId"],
        json!(ids.segment_id)
    );
    assert_eq!(
        execution_detail["preparedSegmentReferences"][0]["revisionId"],
        json!(ids.revision_id)
    );
    assert_eq!(
        execution_detail["technicalFactReferences"][0]["executionId"],
        json!(ids.execution_id)
    );
    assert_eq!(execution_detail["technicalFactReferences"][0]["factId"], json!(ids.fact_id));
    assert_eq!(execution_detail["entityReferences"][0]["executionId"], json!(ids.execution_id));
    assert_eq!(execution_detail["entityReferences"][0]["nodeId"], json!(ids.node_id));
    assert_eq!(execution_detail["relationReferences"][0]["executionId"], json!(ids.execution_id));
    assert_eq!(execution_detail["relationReferences"][0]["edgeId"], json!(ids.edge_id));
}

#[test]
fn non_finalizable_structured_content_does_not_promote_source_titles_to_preserve_spans() {
    let mut detail = sample_execution_detail(ExecutionDetailIds::sequential(21));
    detail.verification_state = AssistantVerificationState::Conflicting;
    detail.answer_disposition = AssistantAnswerDisposition::NonTerminal;

    let answer = "Use `/etc/alpha.ini` with `alphaKey = true`.";
    let structured =
        crate::interfaces::http::mcp::grounded_answer_contract_payload(answer, &detail);
    let structured_content = &structured["structuredContent"];

    assert_eq!(structured_content["finalAnswerReady"], json!(false));
    assert_eq!(structured_content["finalizable"], json!(false));
    assert_eq!(
        structured_content["mustPreserveSpans"],
        json!(["/etc/alpha.ini", "alphaKey = true"])
    );
}

#[test]
fn clarification_is_terminal_without_being_advertised_as_a_factual_answer() {
    let mut detail = sample_execution_detail(ExecutionDetailIds::sequential(28));
    detail.clarification.required = true;
    detail.clarification.question = Some("Which documented variant do you mean?".to_string());
    detail.answer_disposition = AssistantAnswerDisposition::Clarification;

    for profile in [
        crate::mcp_types::McpGroundedAnswerResponseProfile::Full,
        crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
    ] {
        let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload_with_profile(
            "Choose one documented variant.",
            &detail,
            profile,
            Some(3),
        );
        let structured = &payload["structuredContent"];

        assert_eq!(structured["finalAnswerReady"], json!(false));
        assert_eq!(structured["finalizable"], json!(false));
        assert_eq!(structured["readiness"]["clarificationRequired"], json!(true));
        assert_eq!(structured["repairPolicy"]["required"], json!(false));
        assert_eq!(structured["repairPolicy"]["reason"], json!(null));
        assert_eq!(structured["repairPolicy"]["maxAdditionalGroundedAnswerCalls"], json!(0));
    }
}

#[test]
fn empty_answer_is_never_ready_in_either_profile() {
    let detail = sample_execution_detail(ExecutionDetailIds::sequential(81));

    for profile in [
        crate::mcp_types::McpGroundedAnswerResponseProfile::Full,
        crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
    ] {
        let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload_with_profile(
            "   ",
            &detail,
            profile,
            Some(3),
        );
        let structured = &payload["structuredContent"];

        assert_eq!(structured["finalAnswerReady"], json!(false));
        assert_eq!(structured["finalizable"], json!(false));
        assert_eq!(structured["repairPolicy"]["required"], json!(true));
        assert_eq!(structured["repairPolicy"]["reason"], json!("answer_missing"));
    }
}

#[test]
fn blocking_verifier_warning_is_not_advertised_as_a_final_answer() {
    let mut detail = sample_execution_detail(ExecutionDetailIds::sequential(35));
    detail.verification_warnings.push(ironrag_contracts::assistant::AssistantVerificationWarning {
        code: "partial_coverage".to_string(),
        message: "Synthetic coverage warning".to_string(),
        related_segment_id: None,
        related_fact_id: None,
    });
    detail.answer_disposition = AssistantAnswerDisposition::NonTerminal;

    let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload(
        "Only part of the requested inventory is supported.",
        &detail,
    );
    let structured = &payload["structuredContent"];

    assert_eq!(structured["finalAnswerReady"], json!(false));
    assert_eq!(structured["finalizable"], json!(false));
}

#[test]
fn unsupported_literal_state_remains_nonterminal() {
    let mut detail = sample_execution_detail(ExecutionDetailIds::sequential(49));
    detail.verification_state = AssistantVerificationState::InsufficientEvidence;
    detail.answer_disposition = AssistantAnswerDisposition::NonTerminal;
    detail.verification_warnings.push(ironrag_contracts::assistant::AssistantVerificationWarning {
        code: "unsupported_literal".to_string(),
        message: "An exact literal is unsupported.".to_string(),
        related_segment_id: None,
        related_fact_id: None,
    });

    let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload(
        "Candidate with an unsupported exact literal.",
        &detail,
    );
    let structured = &payload["structuredContent"];

    assert_eq!(structured["finalAnswerReady"], json!(false));
    assert_eq!(structured["finalizable"], json!(false));
    assert_eq!(structured["repairPolicy"]["required"], json!(true));
}

#[test]
fn safe_fallback_is_terminal_without_becoming_factual_ready() {
    let mut detail = sample_execution_detail(ExecutionDetailIds::sequential(56));
    detail.verification_state = AssistantVerificationState::InsufficientEvidence;
    detail.answer_disposition = AssistantAnswerDisposition::SafeFallback;

    let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload(
        "A deterministic safe fallback.",
        &detail,
    );
    let structured = &payload["structuredContent"];

    assert_eq!(structured["finalAnswerReady"], json!(false));
    assert_eq!(structured["repairPolicy"]["required"], json!(false));
    assert_eq!(structured["readiness"]["answerDisposition"], json!("safe_fallback"));
}

#[test]
fn finalizer_owned_factual_disposition_accepts_ordinary_prose_states() {
    for state in
        [AssistantVerificationState::NotRun, AssistantVerificationState::PartiallySupported]
    {
        let mut detail = sample_execution_detail(ExecutionDetailIds::sequential(63));
        detail.verification_state = state;
        detail.answer_disposition = AssistantAnswerDisposition::FactualReady;

        let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload(
            "A grounded prose explanation.",
            &detail,
        );
        let structured = &payload["structuredContent"];

        assert_eq!(structured["finalAnswerReady"], json!(true));
        assert_eq!(structured["repairPolicy"]["required"], json!(false));
        assert_eq!(structured["readiness"]["answerDisposition"], json!("factual_ready"));
    }
}

#[test]
fn compact_profile_exposes_readiness_verifier_warnings_and_bounded_references() {
    let mut detail = sample_execution_detail(ExecutionDetailIds::sequential(31));
    detail.verification_warnings.push(ironrag_contracts::assistant::AssistantVerificationWarning {
        code: "synthetic_warning".to_string(),
        message: "Synthetic verifier warning".to_string(),
        related_segment_id: None,
        related_fact_id: None,
    });

    let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload_with_profile(
        "Grounded compact answer.",
        &detail,
        crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
        Some(3),
    );
    let structured = &payload["structuredContent"];

    assert_eq!(structured["answerBody"], json!("Grounded compact answer."));
    assert_eq!(structured["responseProfile"], json!("compact"));
    assert!(structured["executionDetail"].get("chunkReferences").is_none());
    assert_eq!(structured["readiness"]["finalAnswerReady"], json!(true));
    assert_eq!(structured["readiness"]["finalizable"], json!(true));
    assert_eq!(structured["verifier"]["state"], json!("verified"));
    assert_eq!(structured["verifier"]["warningCount"], json!(1));
    assert_eq!(structured["warnings"][0]["code"], json!("synthetic_warning"));
    assert_eq!(structured["referenceSummary"]["totalCount"], json!(5));
    assert_eq!(structured["referenceSummary"]["returnedCount"], json!(3));
    assert_eq!(structured["referenceSummary"]["truncated"], json!(true));
    assert_eq!(structured["referenceSummary"]["references"].as_array().map(Vec::len), Some(3));
}

#[test]
fn full_and_compact_profiles_fail_closed_on_shared_completion_gap() {
    let detail = sample_execution_detail(ExecutionDetailIds::sequential(71));
    let query_ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Release],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: Some(SourceSliceSpec {
            direction: SourceSliceDirection::Tail,
            count: Some(4),
            filter: SourceSliceFilter::ReleaseMarker,
        }),
        retrieval_query: Some("neutral synthetic inventory".to_string()),
        confidence: 1.0,
    };
    let answer = "1. Release 2.0 — improved startup.\n2. Release 1.0 — fixed retries.";

    for profile in [
        crate::mcp_types::McpGroundedAnswerResponseProfile::Full,
        crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
    ] {
        let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload_for_query_ir(
            answer,
            &detail,
            &query_ir,
            profile,
            Some(3),
        );
        let structured = &payload["structuredContent"];

        assert_eq!(structured["queryLanguage"], json!("auto"));
        assert_eq!(structured["finalAnswerReady"], json!(false));
        assert_eq!(structured["finalizable"], json!(false));
        assert_eq!(structured["completion"]["complete"], json!(false));
        assert_eq!(structured["completion"]["reason"], json!("ordered_inventory_incomplete"));
        assert_eq!(structured["completion"]["expected"], json!(4));
        assert_eq!(structured["completion"]["observed"], json!(2));
        assert_eq!(structured["readiness"]["finalAnswerReady"], json!(false));
        assert_eq!(structured["readiness"]["finalizable"], json!(false));
        assert_eq!(structured["readiness"]["completionRequired"], json!(true));
        assert_eq!(structured["repairPolicy"]["required"], json!(true));
        assert_eq!(structured["repairPolicy"]["reason"], json!("ordered_inventory_incomplete"));
        assert_eq!(structured["repairPolicy"]["maxAdditionalGroundedAnswerCalls"], json!(1));
    }
}

#[test]
fn compact_reference_limit_validation_fails_loudly() {
    use crate::mcp_types::McpGroundedAnswerResponseProfile::{Compact, Full};

    assert!(resolve_compact_reference_limit(Compact, None).is_ok());
    assert!(resolve_compact_reference_limit(Compact, Some(1)).is_ok());
    assert!(
        resolve_compact_reference_limit(
            Compact,
            Some(crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT),
        )
        .is_ok()
    );
    assert!(
        resolve_compact_reference_limit(
            Compact,
            Some(crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT + 1),
        )
        .is_err()
    );
    assert!(resolve_compact_reference_limit(Full, Some(1)).is_err());
}

#[test]
fn grounded_answer_input_rejects_unknown_fields_and_invalid_bounds() {
    assert!(
        serde_json::from_value::<GroundedAnswerArgs>(json!({
            "library": "workspace/library",
            "query": "question",
            "top_k": 8
        }))
        .is_err()
    );

    let mut empty = serde_json::from_value::<GroundedAnswerArgs>(json!({
        "library": "   ",
        "query": "   "
    }))
    .expect("parse empty strings before semantic validation");
    assert!(validate_and_normalize_grounded_answer_args(&mut empty).is_err());

    for top_k in [0, MAX_TOP_K + 1] {
        let mut args = serde_json::from_value::<GroundedAnswerArgs>(json!({
            "library": "workspace/library",
            "query": "question",
            "topK": top_k
        }))
        .expect("parse topK");
        assert!(validate_and_normalize_grounded_answer_args(&mut args).is_err());
    }

    let turns = (0..=MCP_GROUNDED_MAX_CONVERSATION_TURNS)
        .map(|_| json!({"role": "user", "content": "context"}))
        .collect::<Vec<_>>();
    let mut too_many_turns = serde_json::from_value::<GroundedAnswerArgs>(json!({
        "library": "workspace/library",
        "query": "question",
        "conversationTurns": turns
    }))
    .expect("parse turn array");
    assert!(validate_and_normalize_grounded_answer_args(&mut too_many_turns).is_err());

    let mut oversized_turn = serde_json::from_value::<GroundedAnswerArgs>(json!({
        "library": "workspace/library",
        "query": "question",
        "conversationTurns": [{
            "role": "assistant",
            "content": "x".repeat(MCP_GROUNDED_MAX_CONVERSATION_TURN_CHARS + 1)
        }]
    }))
    .expect("parse oversized turn");
    assert!(validate_and_normalize_grounded_answer_args(&mut oversized_turn).is_err());
}

#[test]
fn include_debug_rejects_explicit_compact_profile() {
    let mut args = serde_json::from_value::<GroundedAnswerArgs>(json!({
        "library": "workspace/library",
        "query": "question",
        "includeDebug": true,
        "responseProfile": "compact"
    }))
    .expect("parse args");

    assert!(validate_and_normalize_grounded_answer_args(&mut args).is_err());
}

#[test]
fn compact_profile_bounds_large_reference_payloads() {
    let mut detail = sample_execution_detail(ExecutionDetailIds::sequential(41));
    detail.chunk_references = (0..200)
        .map(|index| AssistantChunkReference {
            execution_id: detail.execution.id,
            chunk_id: Uuid::from_u128(1_000 + index as u128),
            rank: index + 1,
            score: 1.0 / f64::from(index + 1),
            document_id: None,
            document_title: None,
            section_path: Vec::new(),
            excerpt: None,
        })
        .collect();

    let full = crate::interfaces::http::mcp::grounded_answer_contract_payload(
        "Grounded compact answer.",
        &detail,
    );
    let compact = crate::interfaces::http::mcp::grounded_answer_contract_payload_with_profile(
        "Grounded compact answer.",
        &detail,
        crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
        None,
    );
    let compact_structured = &compact["structuredContent"];
    let full_bytes = serde_json::to_vec(&full).expect("serialize full payload");
    let compact_bytes = serde_json::to_vec(&compact).expect("serialize compact payload");

    assert_eq!(compact_structured["referenceSummary"]["totalCount"], json!(204));
    assert_eq!(compact_structured["referenceSummary"]["returnedCount"], json!(8));
    assert_eq!(compact_structured["referenceSummary"]["truncated"], json!(true));
    assert!(compact_bytes.len() * 3 < full_bytes.len());
}

/// Named identifier seeds for the synthetic execution-detail fixture, so a
/// caller states which role each id plays instead of relying on positional
/// order.
#[derive(Clone, Copy)]
struct ExecutionDetailIds {
    execution_id: Uuid,
    chunk_id: Uuid,
    segment_id: Uuid,
    revision_id: Uuid,
    fact_id: Uuid,
    node_id: Uuid,
    edge_id: Uuid,
}

impl ExecutionDetailIds {
    /// Fixture convention: `execution_id` is `base`, then each subsequent role
    /// takes the next integer up to `base + 6` (`edge_id`).
    fn sequential(base: u128) -> Self {
        Self {
            execution_id: Uuid::from_u128(base),
            chunk_id: Uuid::from_u128(base + 1),
            segment_id: Uuid::from_u128(base + 2),
            revision_id: Uuid::from_u128(base + 3),
            fact_id: Uuid::from_u128(base + 4),
            node_id: Uuid::from_u128(base + 5),
            edge_id: Uuid::from_u128(base + 6),
        }
    }
}

fn sample_execution_detail(ids: ExecutionDetailIds) -> AssistantExecutionDetail {
    let ExecutionDetailIds {
        execution_id,
        chunk_id,
        segment_id,
        revision_id,
        fact_id,
        node_id,
        edge_id,
    } = ids;
    let now = Utc::now();
    let workspace_id = Uuid::from_u128(11);
    let library_id = Uuid::from_u128(12);
    let conversation_id = Uuid::from_u128(13);
    let context_bundle_id = Uuid::from_u128(16);
    let runtime_execution_id = Uuid::from_u128(17);

    AssistantExecutionDetail {
        context_bundle_id,
        execution: AssistantExecution {
            id: execution_id,
            workspace_id,
            library_id,
            conversation_id,
            context_bundle_id,
            request_turn_id: None,
            response_turn_id: None,
            binding_id: None,
            runtime_execution_id,
            lifecycle_state: "completed".to_string(),
            active_stage: None,
            query_text: "Which endpoint is primary?".to_string(),
            failure_code: None,
            started_at: now,
        },
        runtime_summary: AssistantRuntimeSummary {
            runtime_execution_id,
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
        chunk_references: vec![AssistantChunkReference {
            execution_id,
            chunk_id,
            rank: 1,
            score: 0.91,
            document_id: None,
            document_title: None,
            section_path: Vec::new(),
            excerpt: None,
        }],
        prepared_segment_references: vec![AssistantPreparedSegmentReference {
            execution_id,
            segment_id,
            revision_id,
            block_kind: "endpoint_block".to_string(),
            rank: 2,
            score: 0.82,
            heading_trail: vec!["API".to_string()],
            section_path: vec!["contracts".to_string()],
            document_id: Some(Uuid::from_u128(18)),
            document_title: Some("Synthetic contract".to_string()),
            document_hint: Some("Synthetic contract".to_string()),
            source_access: Some(AssistantContentSourceAccess {
                kind: "stored_document".to_string(),
                href: "urn:synthetic:contract".to_string(),
            }),
        }],
        technical_fact_references: vec![AssistantTechnicalFactReference {
            execution_id,
            fact_id,
            revision_id,
            fact_kind: "endpoint_path".to_string(),
            normalized_value: "/v1/items".to_string(),
            display_value: "/v1/items".to_string(),
            rank: 3,
            score: 0.73,
        }],
        entity_references: vec![AssistantEntityReference {
            execution_id,
            node_id,
            rank: 4,
            score: 0.64,
            label: "Synthetic API".to_string(),
            entity_type: Some("service".to_string()),
            summary: Some("Synthetic API node".to_string()),
        }],
        relation_references: vec![AssistantRelationReference {
            execution_id,
            edge_id,
            rank: 5,
            score: 0.55,
            predicate: "calls".to_string(),
            normalized_assertion: Some("Synthetic API calls endpoint".to_string()),
        }],
        reference_summary: None,
        verification_state: AssistantVerificationState::Verified,
        verification_warnings: Vec::new(),
        answer_disposition: AssistantAnswerDisposition::FactualReady,
        clarification: ironrag_contracts::assistant::AssistantClarification::default(),
    }
}
