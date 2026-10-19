use super::*;

#[test]
fn effective_tool_call_fingerprint_normalizes_defaulted_arguments() {
    let left = effective_tool_call_fingerprint(
        GROUNDED_ANSWER_TOOL_NAME,
        r#"{"query":"focused subquestion"}"#,
        "focused subquestion",
        8,
        "workspace-a/library-b",
        &[],
    )
    .expect("fingerprint");
    let right = effective_tool_call_fingerprint(
        GROUNDED_ANSWER_TOOL_NAME,
        r#"{"topK":8,"library":"workspace-a/library-b","query":"focused subquestion"}"#,
        "focused subquestion",
        8,
        "workspace-a/library-b",
        &[],
    )
    .expect("fingerprint");

    assert_eq!(left, right);
}

#[test]
fn effective_tool_call_fingerprint_keeps_scope_errors_authoritative() {
    let fingerprint = effective_tool_call_fingerprint(
        GROUNDED_ANSWER_TOOL_NAME,
        r#"{"query":"focused subquestion","library":"workspace-x/library-y"}"#,
        "focused subquestion",
        8,
        "workspace-a/library-b",
        &[],
    );

    assert!(fingerprint.is_none());
}

#[test]
fn effective_tool_call_fingerprint_detects_contextual_history_defaults() {
    let history = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "original question".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "original answer".to_string(),
        },
    ];
    let first = effective_tool_call_fingerprint(
        GROUNDED_ANSWER_TOOL_NAME,
        r#"{"library":"workspace-a/library-b","query":"follow-up"}"#,
        "follow-up",
        32,
        "workspace-a/library-b",
        &history,
    )
    .expect("fingerprint");
    let second = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"conversationTurns":[],"topK":24,"query":"follow-up","library":"workspace-a/library-b"}"#,
            "follow-up",
            32,
            "workspace-a/library-b",
            &history,
        )
        .expect("fingerprint");

    assert_eq!(first, second);
}

#[test]
fn duplicate_effective_tool_payload_is_suppressed_before_dispatch() {
    // Two calls with *identical raw arguments* in the same in-flight batch
    // are genuine same-args spam and must still be curbed.
    let raw_arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "focused subquestion",
        "topK": 8
    })
    .to_string();
    let tool_calls = vec![
        ChatToolCall {
            id: "call-1".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: raw_arguments.clone(),
        },
        ChatToolCall {
            id: "call-2".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: raw_arguments,
        },
    ];
    let mut outcomes = vec![None; tool_calls.len()];
    let mut seen = BTreeMap::new();

    let pending = prepare_agent_tool_calls(
        &tool_calls,
        AgentToolArgumentContext {
            user_question: "focused subquestion",
            contextual_follow_up: false,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &[],
        },
        &mut seen,
        &mut outcomes,
    );

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].index, 0);
    assert!(outcomes[0].is_none());
    let duplicate = outcomes[1].as_ref().expect("duplicate outcome");
    assert!(duplicate.is_error);
    assert!(
        duplicate
            .result_text
            .as_deref()
            .is_some_and(|text| text.contains("duplicate MCP tool call suppressed"))
    );
}

#[test]
fn distinct_raw_call_is_not_suppressed_by_in_flight_effective_collision() {
    // The stage bug: a history-bleed call and the clean call normalize to
    // the same effective fingerprint. While the first is still in-flight in
    // the same batch, the distinct-raw clean call must NOT be dead-ended.
    let tool_calls = vec![
            ChatToolCall {
                id: "call-1".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "library": "workspace-a/library-b",
                    "query": "how to update the edge service component? Release 9.7.1 added Alpha Suite cache; Release 9.7.0 shipped Provider Beta connector; Release 9.6.3 patched Gamma module logging",
                    "topK": 8
                })
                .to_string(),
            },
            ChatToolCall {
                id: "call-2".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "library": "workspace-a/library-b",
                    "query": "how to update the edge service component?",
                    "topK": 8
                })
                .to_string(),
            },
        ];
    let user_question = "how to update the edge service component?";
    let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "list the latest ten releases".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "Release 9.7.1 added Alpha Suite cache; Release 9.7.0 shipped Provider Beta connector; Release 9.6.3 patched Gamma module logging".to_string(),
            },
        ];
    let mut outcomes = vec![None; tool_calls.len()];
    let mut seen = BTreeMap::new();

    let pending = prepare_agent_tool_calls(
        &tool_calls,
        AgentToolArgumentContext {
            user_question,
            contextual_follow_up: false,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
        &mut seen,
        &mut outcomes,
    );

    // Sanity: today both calls collapse to the SAME effective fingerprint.
    let fingerprint_a = effective_tool_call_fingerprint(
        GROUNDED_ANSWER_TOOL_NAME,
        &tool_calls[0].arguments_json,
        user_question,
        8,
        "workspace-a/library-b",
        &history,
    )
    .expect("fingerprint a");
    let fingerprint_b = effective_tool_call_fingerprint(
        GROUNDED_ANSWER_TOOL_NAME,
        &tool_calls[1].arguments_json,
        user_question,
        8,
        "workspace-a/library-b",
        &history,
    )
    .expect("fingerprint b");
    assert_eq!(
        fingerprint_a, fingerprint_b,
        "the collision repro requires both calls to normalize identically"
    );
    // But their RAW arguments differ, so the clean call is NOT suppressed.
    assert_ne!(tool_calls[0].arguments_json, tool_calls[1].arguments_json);

    assert_eq!(pending.len(), 2);
    assert!(outcomes[0].is_none());
    assert!(outcomes[1].is_none());
}

#[test]
fn clean_call_runs_after_junk_errored_call_clears_fingerprint() {
    // Cross-iteration shape: the junk history-bleed call errored (its
    // fingerprint is removed), so the later clean call runs unimpeded even
    // though it normalizes to the same effective fingerprint.
    let user_question = "how to update the edge service component?";
    let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "list the latest ten releases".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "Release 9.7.1 added Alpha Suite cache; Release 9.7.0 shipped Provider Beta connector; Release 9.6.3 patched Gamma module logging".to_string(),
            },
        ];
    let junk_call = ChatToolCall {
            id: "call-junk".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "how to update the edge service component? Release 9.7.1 added Alpha Suite cache; Release 9.7.0 shipped Provider Beta connector; Release 9.6.3 patched Gamma module logging",
                "topK": 8
            })
            .to_string(),
        };
    let clean_call = ChatToolCall {
        id: "call-clean".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "how to update the edge service component?",
            "topK": 8
        })
        .to_string(),
    };
    let mut seen = BTreeMap::new();

    let mut first_outcomes = vec![None; 1];
    let first_pending = prepare_agent_tool_calls(
        std::slice::from_ref(&junk_call),
        AgentToolArgumentContext {
            user_question,
            contextual_follow_up: false,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
        &mut seen,
        &mut first_outcomes,
    );
    let fingerprint = first_pending[0].fingerprint.clone().expect("fingerprint");
    let raw_args_key = raw_tool_call_argument_key(&junk_call.arguments_json);
    let error = tool_execution_error("upstream timeout");
    record_effective_tool_payload_outcome(&mut seen, fingerprint, raw_args_key, &error);

    let mut second_outcomes = vec![None; 1];
    let second_pending = prepare_agent_tool_calls(
        std::slice::from_ref(&clean_call),
        AgentToolArgumentContext {
            user_question,
            contextual_follow_up: false,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
        &mut seen,
        &mut second_outcomes,
    );

    assert_eq!(second_pending.len(), 1);
    assert!(second_outcomes[0].is_none());
}
