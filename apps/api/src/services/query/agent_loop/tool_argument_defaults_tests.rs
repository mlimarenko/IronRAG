use super::*;

#[test]
fn ui_agent_raises_contextual_grounded_answer_tool_top_k_floor() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "follow-up subquestion",
        "topK": 4,
        "conversationTurns": [
            {"role": "user", "content": "original question"},
            {"role": "assistant", "content": "original answer"}
        ]
    });

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "follow-up subquestion",
            contextual_follow_up: true,
            grounded_top_k: 32,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &[
                ExternalConversationTurn {
                    turn_kind: QueryTurnKind::User,
                    content_text: "original question".to_string(),
                },
                ExternalConversationTurn {
                    turn_kind: QueryTurnKind::Assistant,
                    content_text: "original answer".to_string(),
                },
            ],
        },
    );

    assert_eq!(arguments["topK"], 24);
}

#[test]
fn ui_agent_raises_injected_contextual_grounded_answer_tool_top_k_floor() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "follow-up subquestion",
        "topK": 4
    });
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

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "follow-up subquestion",
            contextual_follow_up: true,
            grounded_top_k: 32,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    assert_eq!(arguments["topK"], 24);
    assert_eq!(
        arguments["conversationTurns"],
        serde_json::json!([
            {"role": "user", "content": "original question"},
            {"role": "assistant", "content": "original answer"}
        ])
    );
}

#[test]
fn ui_agent_omits_typed_history_for_non_contextual_grounded_answer() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "new standalone topic",
        "topK": 4,
        "conversationTurns": []
    });
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::User,
        content_text: "previous topic".to_string(),
    }];

    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        "new standalone topic",
        32,
        "workspace-a/library-b",
        &history,
    );

    assert_eq!(arguments["topK"], 24);
    assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
}

#[test]
fn ui_agent_keeps_grounded_answer_query_at_the_current_question() {
    let user_question =
        "AlphaZero BetaOne GammaTwo DeltaThree EpsilonFour ZetaFive EtaSix ThetaSeven";
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "AlphaZero GammaTwo DeltaThree",
        "topK": 5
    });

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question,
            contextual_follow_up: false,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &[],
        },
    );

    assert_eq!(arguments["query"], user_question);
}

#[test]
fn ui_agent_restores_single_grounded_answer_query_with_missing_user_terms() {
    let user_question = "AlphaOne BetaTwo GammaThree DeltaFour EpsilonFive ZetaSix";
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "AlphaOne BetaTwo GammaThree DeltaFour",
        "topK": 5
    });

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question,
            contextual_follow_up: false,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &[],
        },
    );

    assert_eq!(arguments["query"], user_question);
}

#[test]
fn ui_agent_restores_single_grounded_answer_query_with_similar_length_paraphrase() {
    let user_question = "AlphaOne BetaTwo GammaThree DeltaFour EpsilonFive ZetaSix EtaSeven";
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "AlphaOne BetaTwo GammaThree DeltaFour EpsilonNine ZetaTen",
        "topK": 5
    });

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question,
            contextual_follow_up: false,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &[],
        },
    );

    assert_eq!(arguments["query"], user_question);
}

#[test]
fn ui_agent_defaults_grounded_answer_conversation_turns_from_typed_history() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "follow-up question"
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
fn ui_agent_drops_explicit_grounded_answer_conversation_turns_without_server_history() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "follow-up question",
        "conversationTurns": [
            {"role": "user", "content": "model supplied context"}
        ]
    });
    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        "follow-up question",
        8,
        "workspace-a/library-b",
        &[],
    );

    assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
}

#[test]
fn ui_agent_replaces_stale_explicit_grounded_answer_conversation_turns_with_server_history() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "show full ready config",
        "conversationTurns": [
            {"role": "user", "content": "how do I configure connector alpha"},
            {"role": "assistant", "content": "Connector Alpha uses `alphaFlag`."},
            {"role": "user", "content": "configure it"}
        ]
    });
    let history = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "configure it".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Install `pkg-alpha`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                .to_string(),
        },
    ];

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "show full ready config",
            contextual_follow_up: true,
            grounded_top_k: 8,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    assert_eq!(
        arguments["conversationTurns"],
        serde_json::json!([
            {"role": "user", "content": "configure it"},
            {
                "role": "assistant",
                "content": "Install `pkg-alpha`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
            }
        ])
    );
}

#[test]
fn ui_agent_drops_model_requested_grounded_answer_history_without_follow_up_gate() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "show full ready config",
        "conversationTurns": [
            {"role": "user", "content": "model supplied context"}
        ]
    });
    let history = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "configure Connector Alpha".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Install `pkg-alpha`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                .to_string(),
        },
    ];

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "show full ready config",
            contextual_follow_up: false,
            grounded_top_k: 32,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
    assert_eq!(arguments["topK"], 24);
}

#[test]
fn ui_agent_ignores_llm_supplied_literal_padding_without_server_history() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "focused subquestion",
        "topK": 4,
        "conversationTurns": [
            {
                "role": "assistant",
                "content": "`fake-a` `fake-b` `fake-c` `fake-d` `fake-e` `fake-f` `fake-g` `fake-h`"
            }
        ]
    });

    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        "focused subquestion",
        24,
        "workspace-a/library-b",
        &[],
    );

    assert_eq!(arguments["topK"], 24);
    assert_eq!(arguments["query"], "focused subquestion");
    assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
}

#[test]
fn ui_agent_drops_explicit_empty_grounded_answer_conversation_turns_for_standalone() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "new standalone topic",
        "conversationTurns": []
    });
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::User,
        content_text: "previous topic".to_string(),
    }];

    apply_agent_tool_argument_defaults(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        "new standalone topic",
        8,
        "workspace-a/library-b",
        &history,
    );

    assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
}

#[test]
fn ui_agent_drops_model_requested_history_for_non_contextual_grounded_answer() {
    let mut arguments = serde_json::json!({
        "library": "workspace-a/library-b",
        "query": "Which Alpha service workers connect to the message bus and what URL format is used?",
        "conversationTurns": [
            {"role": "user", "content": "previous model supplied topic"},
            {"role": "assistant", "content": "previous model supplied answer"}
        ]
    });
    let history = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "Alpha service setup inventory".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "`alpha-api` and `alpha-worker` expose local runtime settings."
                .to_string(),
        },
    ];

    apply_agent_tool_argument_defaults_with_context(
        GROUNDED_ANSWER_TOOL_NAME,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: "Which Alpha service workers connect to the message bus and what URL format is used?",
            contextual_follow_up: false,
            grounded_top_k: 24,
            library_ref: "workspace-a/library-b",
            grounded_answer_tool_history: &history,
        },
    );

    assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
}
