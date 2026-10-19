use super::*;

#[test]
fn explicit_conversation_title_is_trimmed_and_collapsed() {
    let title = normalize_explicit_conversation_title("  Durable\n  session   title  ")
        .expect("bounded non-empty title");

    assert_eq!(title, "Durable session title");
}

#[test]
fn explicit_conversation_title_rejects_empty_input() {
    let error = normalize_explicit_conversation_title(" \n\t ").expect_err("empty title");

    assert!(matches!(error, crate::interfaces::http::router_support::ApiError::BadRequest(_)));
}

#[test]
fn explicit_conversation_title_rejects_more_than_the_contract_limit() {
    let over_limit = "x".repeat(super::QUERY_CONVERSATION_TITLE_LIMIT + 1);
    let error = normalize_explicit_conversation_title(&over_limit).expect_err("oversized title");

    assert!(matches!(error, crate::interfaces::http::router_support::ApiError::BadRequest(_)));
}

#[test]
fn derived_conversation_title_includes_ellipsis_within_the_contract_limit() {
    let source = "x".repeat(super::QUERY_CONVERSATION_TITLE_LIMIT + 5);
    let title = derive_conversation_title(&source).expect("non-empty derived title");

    assert_eq!(title.chars().count(), super::QUERY_CONVERSATION_TITLE_LIMIT);
    assert!(title.ends_with('…'));
}

#[test]
fn build_conversation_runtime_context_exports_bounded_typed_grounded_answer_tool_history() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "which connector variants exist".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let system_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::System,
        author_principal_id: None,
        content_text: "internal lifecycle note".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let tool_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::Tool,
        author_principal_id: None,
        content_text: "tool observation".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 4,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Connector Alpha uses `alphaSecret`.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 5,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "and limitations?".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, system_turn, tool_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    assert_eq!(context.grounded_answer_tool_history.len(), 2);
    assert!(matches!(context.grounded_answer_tool_history[0].turn_kind, QueryTurnKind::User));
    assert!(matches!(context.grounded_answer_tool_history[1].turn_kind, QueryTurnKind::Assistant));
    assert!(context.grounded_answer_tool_history.iter().all(|turn| {
        !turn.content_text.contains("System note")
            && !turn.content_text.contains("Tool observation")
            && !turn.content_text.contains("internal lifecycle note")
            && !turn.content_text.contains("tool observation")
    }));
}

#[test]
fn build_conversation_runtime_context_keeps_short_disambiguator_for_tool_follow_up() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "how do I configure account connectors".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_choice_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Available variants: Provider Alpha, Provider Beta.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let disambiguator_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "Provider Alpha".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_detail_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 4,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Provider Alpha uses `/opt/provider-alpha/connector.conf` with `alphaUrl`."
            .to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let second_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 5,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "show how to configure it".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let second_assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 6,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Set `alphaSecret` and `alphaTimeout`.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 7,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "ready config please".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[
            first_user_turn,
            assistant_choice_turn,
            disambiguator_turn,
            assistant_detail_turn,
            second_user_turn,
            second_assistant_turn,
            follow_up_turn.clone(),
        ],
        follow_up_turn.id,
    );

    let history_text = context
        .grounded_answer_tool_history
        .iter()
        .map(|turn| turn.content_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(history_text.contains("Provider Alpha"));
    assert!(history_text.contains("/opt/provider-alpha/connector.conf"));
    assert!(history_text.contains("alphaSecret"));
    assert_eq!(context.grounded_answer_tool_history.len(), 6);
}

#[test]
fn build_conversation_runtime_context_preserves_dense_assistant_literals_in_tool_history() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "which package-like modules exist".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: format!(
            "`pkg-alpha` `pkg-beta` `pkg-gamma` `pkg-delta` {} `pkg-epsilon` `pkg-zeta` `pkg-eta` `pkg-theta`",
            "long filler ".repeat(260)
        ),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "describe each item".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    let assistant_history = context
        .grounded_answer_tool_history
        .iter()
        .find(|turn| {
            matches!(turn.turn_kind, QueryTurnKind::Assistant)
                && turn.content_text.starts_with("ir.memory.literals.v1: ")
        })
        .expect("assistant history should be exported");
    assert!(assistant_history.content_text.starts_with("ir.memory.literals.v1: "));
    assert!(assistant_history.content_text.contains("`pkg-alpha`"));
    assert!(assistant_history.content_text.contains("`pkg-theta`"));
    assert!(
        assistant_history.content_text.chars().count() < 900,
        "dense assistant history should preserve exact literals without replaying most prior prose"
    );

    let prompt_history_message = context
        .prompt_history_messages
        .iter()
        .find(|message| {
            message.content.as_deref().is_some_and(|content| {
                content.contains("ir.context.compact-literal-memory.v1:")
                    && content.contains("`pkg-alpha`")
            })
        })
        .expect("compact literal memory should reach prompt history");
    assert_eq!(prompt_history_message.role, "system");
    let prompt_history_text = prompt_history_message.content.as_deref().unwrap_or_default();
    assert!(prompt_history_text.contains("ir.memory.literals.v1: "));
    assert!(prompt_history_text.starts_with("ir.context.compact-literal-memory.v1:"));
}

#[test]
fn build_conversation_runtime_context_preserves_long_path_literals_in_tool_history() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "how do I configure Provider Alpha".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: format!(
            "Use `/opt/provider-alpha/configuration/files/provider-alpha-primary.conf` and set `alphaSecret` `alphaUrl` `alphaTimeout` `alphaMode` `alphaToken` `alphaCurrency` `alphaRetries`. {}",
            "long filler ".repeat(260)
        ),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "show the config".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    let assistant_history = context
        .grounded_answer_tool_history
        .iter()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .expect("assistant history should be exported");
    assert!(
        assistant_history
            .content_text
            .contains("`/opt/provider-alpha/configuration/files/provider-alpha-primary.conf`")
    );
}

#[test]
fn build_conversation_runtime_context_separates_external_history_from_current_text() {
    let conversation_id = Uuid::now_v7();
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "now enumerate the integration variants and missing limits".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::User,
        content_text: "what does the library say about payment setup".to_string(),
    }];

    let context = build_conversation_runtime_context_with_external_history(
        std::slice::from_ref(&current_turn),
        current_turn.id,
        &external_prior_turns,
    );

    assert_eq!(context.query_compiler_history.len(), 1);
    assert_eq!(context.query_compiler_history[0].turn_kind, QueryTurnKind::User);
    assert_eq!(
        context.query_compiler_history[0].content_text,
        "what does the library say about payment setup"
    );
    assert_eq!(
        context.current_question_text,
        "now enumerate the integration variants and missing limits"
    );
    assert!(context.has_prior_conversation);
}

#[test]
fn build_conversation_runtime_context_keeps_compacted_literals_case_insensitive() {
    let conversation_id = Uuid::now_v7();
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "describe the settings".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text:
            "`AlphaKey` `alphakey` `BetaKey` `GammaKey` `DeltaKey` `EpsilonKey` `ZetaKey` `EtaKey` `ThetaKey`"
                .to_string(),
    }];

    let context = build_conversation_runtime_context_with_external_history(
        std::slice::from_ref(&current_turn),
        current_turn.id,
        &external_prior_turns,
    );

    let assistant_history = context
        .grounded_answer_tool_history
        .iter()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .expect("assistant tool history");
    let literal_line = assistant_history
        .content_text
        .lines()
        .find(|line| line.starts_with("ir.memory.literals.v1:"))
        .expect("compacted literal summary");
    assert!(literal_line.contains("`AlphaKey`"));
    assert!(
        !literal_line.contains("`alphakey`"),
        "compact literal summary keeps the first spelling only"
    );
}

#[test]
fn prior_grounded_answer_context_messages_preserve_chunk_evidence() {
    let library_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let higher_rank_chunk_id = Uuid::now_v7();
    let reference = test_chunk_reference(chunk_id, 2, 0.92);
    let higher_rank_reference = test_chunk_reference(higher_rank_chunk_id, 1, 0.88);
    let chunk = test_chunk_row(
        library_id,
        chunk_id,
        "Connector Alpha configuration uses [Alpha]\nalphaSecret = ${ALPHA_SECRET}\nalphaMode = strict",
        &["Setup", "Configuration"],
    );
    let higher_rank_chunk = test_chunk_row(
        library_id,
        higher_rank_chunk_id,
        "Connector Alpha package is alpha-connector.",
        &["Setup", "Package"],
    );

    let replay = build_prior_grounded_answer_context_messages(
        library_id,
        execution_id,
        "How is Connector Alpha configured?",
        &[reference, higher_rank_reference],
        &[chunk, higher_rank_chunk],
        4_000,
    )
    .expect("prior grounded answer replay");

    assert_eq!(replay.messages.len(), 1);
    assert_eq!(replay.chunk_ids, vec![higher_rank_chunk_id, chunk_id]);
    let message = &replay.messages[0];
    assert_eq!(message.role, "system");
    assert!(message.tool_calls.is_empty());
    assert!(message.tool_call_id.is_none());
    assert!(message.name.is_none());
    let content = message.content.as_deref().expect("context content");
    assert!(content.contains("Earlier grounded answer evidence"));
    assert!(content.contains(&chunk_id.to_string()));
    assert!(content.contains("section: Setup > Configuration"));
    assert!(content.contains("alphaSecret"));
    let package_position = content.find("alpha-connector").expect("package chunk");
    let config_position = content.find("alphaSecret").expect("config chunk");
    assert!(package_position < config_position);
}

#[test]
fn prior_grounded_answer_context_messages_filter_foreign_library_chunks() {
    let library_id = Uuid::now_v7();
    let foreign_library_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let kept_chunk_id = Uuid::now_v7();
    let foreign_chunk_id = Uuid::now_v7();
    let references = vec![
        test_chunk_reference(foreign_chunk_id, 1, 0.99),
        test_chunk_reference(kept_chunk_id, 2, 0.88),
    ];
    let chunks = vec![
        test_chunk_row(
            foreign_library_id,
            foreign_chunk_id,
            "Foreign connector setting must not cross library boundaries.",
            &["Foreign"],
        ),
        test_chunk_row(
            library_id,
            kept_chunk_id,
            "Connector Beta keeps betaWindow in [Beta].",
            &["Setup", "Beta"],
        ),
    ];

    let replay = build_prior_grounded_answer_context_messages(
        library_id,
        execution_id,
        "How is Connector Beta configured?",
        &references,
        &chunks,
        4_000,
    )
    .expect("filtered prior grounded answer replay");

    assert_eq!(replay.chunk_ids, vec![kept_chunk_id]);
    let content = replay.messages[0].content.as_deref().expect("context content");
    assert!(content.contains("betaWindow"));
    assert!(!content.contains("Foreign connector"));
    assert!(!content.contains(&foreign_chunk_id.to_string()));
}

#[test]
fn prior_grounded_answer_replay_selection_preserves_newest_first_order() {
    let library_id = Uuid::now_v7();
    let foreign_library_id = Uuid::now_v7();
    let latest_id = Uuid::now_v7();
    let older_id = Uuid::now_v7();
    let failed_id = Uuid::now_v7();
    let foreign_id = Uuid::now_v7();
    let now = Utc::now();
    let executions = vec![
        test_query_execution_row(
            library_id,
            latest_id,
            RuntimeLifecycleState::Completed,
            None,
            "latest precise setup question",
            now,
        ),
        test_query_execution_row(
            library_id,
            failed_id,
            RuntimeLifecycleState::Failed,
            Some("tool_error"),
            "failed setup question",
            now,
        ),
        test_query_execution_row(
            foreign_library_id,
            foreign_id,
            RuntimeLifecycleState::Completed,
            None,
            "foreign library question",
            now,
        ),
        test_query_execution_row(
            library_id,
            older_id,
            RuntimeLifecycleState::Completed,
            None,
            "older broad setup question",
            now,
        ),
    ];

    let selected = select_prior_grounded_answer_replay_executions(executions, library_id, 2);

    let selected_ids = selected.iter().map(|execution| execution.id).collect::<Vec<_>>();
    assert_eq!(selected_ids, vec![latest_id, older_id]);
}

fn test_chunk_reference(chunk_id: Uuid, rank: i32, score: f64) -> KnowledgeBundleChunkReferenceRow {
    KnowledgeBundleChunkReferenceRow {
        bundle_id: Uuid::now_v7(),
        chunk_id,
        rank,
        score,
        inclusion_reason: Some("synthetic".to_string()),
        created_at: Utc::now(),
    }
}

fn test_query_execution_row(
    library_id: Uuid,
    execution_id: Uuid,
    runtime_lifecycle_state: RuntimeLifecycleState,
    failure_code: Option<&str>,
    query_text: &str,
    started_at: chrono::DateTime<Utc>,
) -> query_repository::QueryExecutionRow {
    query_repository::QueryExecutionRow {
        id: execution_id,
        workspace_id: Uuid::now_v7(),
        library_id,
        conversation_id: Uuid::now_v7(),
        context_bundle_id: Uuid::now_v7(),
        request_turn_id: Some(Uuid::now_v7()),
        response_turn_id: Some(Uuid::now_v7()),
        binding_id: Some(Uuid::now_v7()),
        runtime_execution_id: Uuid::now_v7(),
        runtime_lifecycle_state,
        runtime_active_stage: None,
        turn_budget: 5,
        turn_count: 1,
        parallel_action_limit: 3,
        query_text: query_text.to_string(),
        failure_code: failure_code.map(str::to_string),
        failure_summary_redacted: None,
        started_at,
        runtime_accepted_at: started_at,
        runtime_completed_at: Some(started_at),
    }
}

#[test]
fn runtime_summary_view_does_not_expose_persisted_diagnostic_or_query_text() {
    let library_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let now = Utc::now();
    let private_query = "view-sentinel-private-query";
    let private_diagnostic = "view-sentinel-secret-diagnostic";
    let mut row = test_query_execution_row(
        library_id,
        execution_id,
        RuntimeLifecycleState::Failed,
        Some("query_provider_failed"),
        private_query,
        now,
    );
    row.failure_summary_redacted = Some(format!("{private_diagnostic}: {private_query}"));

    let summary = map_execution_runtime_summary(&row, &[]);
    let exposed_json = serde_json::to_string(&summary).expect("serialize runtime summary view");

    assert_eq!(summary.failure_summary_redacted.as_deref(), Some("query_provider_failed"));
    assert!(!exposed_json.contains(private_diagnostic));
    assert!(!exposed_json.contains(private_query));
}

fn test_chunk_row(
    library_id: Uuid,
    chunk_id: Uuid,
    content_text: &str,
    heading_trail: &[&str],
) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        chunk_id,
        workspace_id: Uuid::now_v7(),
        library_id,
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("text".to_string()),
        content_text: content_text.to_string(),
        normalized_text: content_text.to_string(),
        span_start: None,
        span_end: None,
        token_count: None,
        support_block_ids: Vec::new(),
        section_path: heading_trail.iter().map(|value| (*value).to_string()).collect(),
        heading_trail: heading_trail.iter().map(|value| (*value).to_string()).collect(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: None,
        window_text: None,
        occurred_at: None,
        occurred_until: None,
    }
}
