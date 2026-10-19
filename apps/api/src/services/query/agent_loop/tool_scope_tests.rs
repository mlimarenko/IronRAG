use super::*;

#[test]
fn ui_agent_strips_prior_subject_from_non_contextual_grounded_answer_query() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "inventory latest ten release entries for ConnectorAlpha and alphaSecret changes",
        "conversationTurns": []
    });
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text: "ConnectorAlpha setup uses `alphaSecret` and `/opt/alpha/connector.conf`."
            .to_string(),
    }];

    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        "inventory latest ten release entries",
        24,
        "workspace-a/library-b",
        &history,
    );

    assert_eq!(arguments["query"], "inventory latest ten release entries");
    assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
}

#[test]
fn ui_agent_overrides_explicit_empty_grounded_answer_conversation_turns_for_follow_up() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "follow-up question",
        "conversationTurns": []
    });
    let history = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "original user question".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "original assistant answer".to_string(),
        },
    ];

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "follow-up question",
            contextual_follow_up: true,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    assert_eq!(
        arguments["conversationTurns"],
        serde_json::json!([
            {"role": "user", "content": "original user question"},
            {"role": "assistant", "content": "original assistant answer"}
        ])
    );
}

#[test]
fn ui_agent_does_not_reclassify_or_reconstruct_grounded_query_from_history() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "Q: [S0], k0, https://localhost/api, [X.f0], http://localhost, k1, merchantId0, cred0, p0-module, staticId0, staticPayload0, codeTtl0, /opt/p0/p0.conf, /var/log/p0.log",
        "conversationTurns": []
    });
    let history = vec![
        ExternalConversationTurn { turn_kind: QueryTurnKind::User, content_text: "Q0".to_string() },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "`p0-module` `/opt/p0/p0.conf` `cred0`".to_string(),
        },
    ];

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "Q",
            contextual_follow_up: true,
            grounded_top_k: 24,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    assert_eq!(arguments["query"], "Q");
    assert_eq!(
        arguments["conversationTurns"],
        serde_json::json!([
            {"role": "user", "content": "Q0"},
            {
                "role": "assistant",
                "content": "`p0-module` `/opt/p0/p0.conf` `cred0`"
            }
        ])
    );
}

#[test]
fn ui_agent_keeps_user_supplied_long_grounded_answer_query() {
    let user_question = "Q: [S0], k0, https://localhost/api, [X.f0], http://localhost, k1, merchantId0, cred0, p0-module, staticId0, staticPayload0, codeTtl0, /opt/p0/p0.conf, /var/log/p0.log";
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": user_question
    });
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text: "A0".to_string(),
    }];

    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        user_question,
        24,
        "workspace-a/library-b",
        &history,
    );

    assert_eq!(arguments["query"], user_question);
}

#[test]
fn ui_agent_rejects_model_rewrite_even_when_it_contains_new_literals() {
    let user_question = "Q";
    let rewritten_query = "Q: `/opt/p1/p1.conf`, `[S0]`, `cred0`, `tok0`, `https://p1.local/api`, `k1 = 30`, `p1-module`";
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": rewritten_query
    });
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text: "A0".to_string(),
    }];

    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        user_question,
        24,
        "workspace-a/library-b",
        &history,
    );

    assert_eq!(arguments["query"], user_question);
}

#[test]
fn ui_agent_passes_prior_code_literals_only_through_typed_conversation_history() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "Q: [S0], [X.f0], https://p0.local/api, k0, k1, k2, fn0, cred0, codeTtl0, /opt/p0/p0.conf, staticId0, staticPayload0, qrId0, qrPayload0, qrTtl0, qrPoll0, qrStatusTtl0, /var/log/p0.log",
        "conversationTurns": []
    });
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text: "`k0` `k1` `k2` `fn0` `cred0` `codeTtl0` `/opt/p0/p0.conf`".to_string(),
    }];

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "Q",
            contextual_follow_up: true,
            grounded_top_k: 24,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    assert_eq!(arguments["query"], "Q");
    assert_eq!(
        arguments["conversationTurns"],
        serde_json::json!([{
            "role": "assistant",
            "content": "`k0` `k1` `k2` `fn0` `cred0` `codeTtl0` `/opt/p0/p0.conf`"
        }])
    );
}

#[test]
fn ui_agent_omits_empty_literal_anchor_tail_when_budget_is_exhausted() {
    let user_question = format!("Q {}", "segment ".repeat(140));
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": format!(
            "{} [S0] [X.f0] https://p0.local/api p0-module /opt/p0/p0.conf cred0 staticId0 staticPayload0",
            user_question
        ),
        "conversationTurns": []
    });
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text: "`p0-module` `/opt/p0/p0.conf` `cred0` `staticId0`".to_string(),
    }];

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: &user_question,
            contextual_follow_up: true,
            grounded_top_k: 24,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    let compact_query = arguments["query"].as_str().unwrap();
    assert_eq!(compact_query, user_question);
    assert!(!compact_query.contains("\n@:"));
}

#[test]
fn ui_agent_bounds_high_fanout_tool_limits() {
    let mut missing = serde_json::json!({
        "library": "workspace-x/library-y",
        "query": "focused probe"
    });
    apply_agent_tool_argument_defaults(
        "search_entities",
        &mut missing,
        "focused probe",
        8,
        "workspace-a/library-b",
        &[],
    );
    assert_eq!(missing["limit"], 8);
    assert_eq!(missing["library"], "workspace-a/library-b");

    let mut wider = serde_json::json!({
        "library": "workspace-a/library-b",
        "limit": 200
    });
    apply_agent_tool_argument_defaults(
        "get_graph_topology",
        &mut wider,
        "",
        12,
        "workspace-a/library-b",
        &[],
    );
    assert_eq!(wider["limit"], 12);

    let mut narrower = serde_json::json!({
        "library": "workspace-a/library-b",
        "limit": 4
    });
    apply_agent_tool_argument_defaults(
        "list_relations",
        &mut narrower,
        "",
        12,
        "workspace-a/library-b",
        &[],
    );
    assert_eq!(narrower["limit"], 4);
}

#[test]
fn ui_agent_forces_search_documents_to_session_library_scope() {
    let mut arguments = serde_json::json!({
        "query": "focused probe",
        "limit": 99
    });

    apply_agent_tool_argument_defaults(
        SEARCH_DOCUMENTS_TOOL_NAME,
        &mut arguments,
        "focused probe",
        8,
        "workspace-a/library-b",
        &[],
    );

    assert_eq!(arguments["libraries"], serde_json::json!(["workspace-a/library-b"]));
    assert_eq!(arguments["limit"], 8);
}

#[test]
fn ui_agent_does_not_rewrite_search_query_from_raw_history() {
    let mut arguments = serde_json::json!({
        "query": "settings parameters",
        "limit": 5
    });
    let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "ir.memory.literals.v1: `alpha-package`, `/etc/alpha.ini`, `[Main]`, `retryTimeout`, `plainword`\nUse `alpha-package` and `retryTimeout`."
                .to_string(),
        }];

    apply_agent_tool_argument_defaults_with_context(
        SEARCH_DOCUMENTS_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "explain all parameters",
            contextual_follow_up: true,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    assert_eq!(arguments["query"], "settings parameters");
    assert_eq!(arguments["libraries"], serde_json::json!(["workspace-a/library-b"]));
}

#[test]
fn ui_agent_does_not_pad_self_sufficient_contextual_search_query() {
    let mut arguments = serde_json::json!({
        "query": "alphaPackage retryTimeout settings parameters",
        "limit": 5
    });
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text:
            "Choose one: `alphaPackage`, `betaImage`, `/etc/alpha.ini`, `[Main]`, `retryTimeout`."
                .to_string(),
    }];

    apply_agent_tool_argument_defaults_with_context(
        SEARCH_DOCUMENTS_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "show settings parameters",
            contextual_follow_up: true,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    assert_eq!(arguments["query"], "alphaPackage retryTimeout settings parameters");
    assert_eq!(arguments["libraries"], serde_json::json!(["workspace-a/library-b"]));
}

#[test]
fn ui_agent_keeps_search_documents_query_without_contextual_anchors() {
    let mut arguments = serde_json::json!({
        "query": "settings parameters",
        "limit": 5
    });

    apply_agent_tool_argument_defaults(
        SEARCH_DOCUMENTS_TOOL_NAME,
        &mut arguments,
        "settings parameters",
        8,
        "workspace-a/library-b",
        &[],
    );

    assert_eq!(arguments["query"], "settings parameters");
}

#[test]
fn ui_agent_normalizes_stringified_search_document_scope_arguments() {
    let mut arguments = serde_json::json!({
        "query": "focused probe",
        "libraries": "[\"workspace-a/library-b\"]",
        "limit": "10"
    });

    normalize_agent_tool_argument_types(SEARCH_DOCUMENTS_TOOL_NAME, &mut arguments);

    validate_agent_tool_library_scope(
        SEARCH_DOCUMENTS_TOOL_NAME,
        &arguments,
        "workspace-a/library-b",
    )
    .expect("normalized scope");
    apply_agent_tool_argument_defaults(
        SEARCH_DOCUMENTS_TOOL_NAME,
        &mut arguments,
        "focused probe",
        8,
        "workspace-a/library-b",
        &[],
    );

    assert_eq!(arguments["libraries"], serde_json::json!(["workspace-a/library-b"]));
    assert_eq!(arguments["limit"], 8);
}

#[test]
fn ui_agent_rejects_cross_library_stringified_search_document_scope() {
    let mut arguments = serde_json::json!({
        "query": "focused probe",
        "libraries": "[\"workspace-x/library-y\"]"
    });

    normalize_agent_tool_argument_types(SEARCH_DOCUMENTS_TOOL_NAME, &mut arguments);
    let error = validate_agent_tool_library_scope(
        SEARCH_DOCUMENTS_TOOL_NAME,
        &arguments,
        "workspace-a/library-b",
    )
    .expect_err("scope mismatch");

    assert!(error.contains("library scope mismatch"));
    assert!(error.contains("workspace-x/library-y"));
    assert!(error.contains("workspace-a/library-b"));
}

#[test]
fn ui_dispatcher_rejects_tools_hidden_from_the_answer_contract() {
    let allowed = BTreeSet::from([
        GROUNDED_ANSWER_TOOL_NAME.to_string(),
        SEARCH_DOCUMENTS_TOOL_NAME.to_string(),
    ]);

    assert!(validate_ui_agent_tool_allowed(GROUNDED_ANSWER_TOOL_NAME, &allowed).is_ok());
    for hidden in ["delete_document", "create_documents", "view_document_image"] {
        let error = validate_ui_agent_tool_allowed(hidden, &allowed)
            .expect_err("hidden tool must be rejected");
        assert!(error.contains("not available"));
        assert!(error.contains(hidden));
    }
}

#[test]
fn ui_agent_rejects_cross_library_single_scope_arguments() {
    let arguments = serde_json::json!({
        "library": "workspace-x/library-y",
        "query": "focused probe"
    });

    let error = validate_agent_tool_library_scope(
        GROUNDED_ANSWER_TOOL_NAME,
        &arguments,
        "workspace-a/library-b",
    )
    .expect_err("scope mismatch");

    assert!(error.contains("library scope mismatch"));
    assert!(error.contains("workspace-x/library-y"));
    assert!(error.contains("workspace-a/library-b"));
}

#[test]
fn ui_agent_rejects_cross_library_search_scope_arguments() {
    let arguments = serde_json::json!({
        "query": "focused probe",
        "libraries": ["workspace-a/library-b", "workspace-x/library-y"]
    });

    let error = validate_agent_tool_library_scope(
        SEARCH_DOCUMENTS_TOOL_NAME,
        &arguments,
        "workspace-a/library-b",
    )
    .expect_err("scope mismatch");

    assert!(error.contains("library scope mismatch"));
    assert!(error.contains("workspace-x/library-y"));
}
