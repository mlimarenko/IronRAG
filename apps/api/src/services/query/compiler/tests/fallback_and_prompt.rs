use super::*;

/// The fallback reports an undecided language so the answer stage keeps
/// the soft "user's language" rule. Pinning English here would answer a
/// Russian question in English precisely when the runtime is degraded.
#[test]
fn provider_free_fallback_leaves_the_answer_language_undecided() {
    let fallback = super::provider_free_fallback_query_ir("как настроить коннектор?");
    assert_eq!(fallback.language, crate::domains::query_ir::QueryLanguage::Auto);
    assert!(
        crate::services::query::assistant_prompt::render_single_shot(fallback.language)
            .contains("* Answer in the user's language.")
    );
}

#[test]
fn provider_free_fallback_is_semantically_neutral() {
    let question = "∆ qlorm vexu 5?";
    let ir = provider_free_fallback_query_ir(question);

    assert_eq!(ir.act, QueryAct::Describe);
    assert_eq!(ir.scope, QueryScope::MultiDocument);
    assert_eq!(ir.language, QueryLanguage::Auto);
    assert!(ir.target_types.is_empty());
    assert!(ir.target_entities.is_empty());
    assert!(ir.source_slice.is_none());
    assert_eq!(ir.effective_retrieval_query("unused"), question);
    assert!(ir.needs_clarification.is_none());
}

#[test]
fn provider_free_fallback_preserves_only_structural_evidence() {
    let question = "∆ `urn:fixture:item-17` /spec/v2.3.4 NODE_9";
    let ir = provider_free_fallback_query_ir(question);

    assert_eq!(ir.act, QueryAct::Describe);
    assert_eq!(ir.language, QueryLanguage::Auto);
    assert!(ir.target_types.is_empty());
    assert!(ir.target_entities.is_empty());
    assert!(ir.source_slice.is_none());
    for expected in ["urn:fixture:item-17", "/spec/v2.3.4", "NODE_9"] {
        assert!(
            ir.literal_constraints.iter().any(|literal| literal.text == expected),
            "missing structural literal {expected:?} in {:?}",
            ir.literal_constraints
        );
    }
}

#[test]
fn provider_free_fallback_never_infers_entities_from_surface_shape() {
    for question in [
        "∆ qlorm Nimbus",
        "NODE_9 status",
        "CamelCaseSubject details",
        "service-name rollout",
        "API:v2 overview",
    ] {
        let ir = provider_free_fallback_query_ir(question);
        assert!(
            ir.target_entities.is_empty(),
            "provider-free fallback must not classify entity semantics from {question:?}"
        );
    }
}

#[tokio::test]
async fn history_turns_are_embedded_in_prompt() {
    let ir_json = serde_json::to_string(&compiled_ir()).unwrap();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();
    let history = vec![
        CompileHistoryTurn {
            role: "user".to_string(),
            content: "do we have a payment module?".to_string(),
        },
        CompileHistoryTurn {
            role: "assistant".to_string(),
            content: "Yes, the payment module is documented.".to_string(),
        },
    ];

    let _ = service
        .compile_with_gateway(&gateway, &binding, "how do I configure it?", &history)
        .await
        .expect("compile ok");

    let prompt = gateway.last_request.lock().unwrap().clone().unwrap().prompt.clone();
    assert!(prompt.contains("Prior conversation"));
    assert!(prompt.contains("payment module"));
    assert!(prompt.contains("how do I configure it?"));
}

#[test]
fn compiler_prompt_preserves_prior_act_for_short_disambiguator() {
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("short disambiguating selection"));
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("keep the prior request's act"));
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("do not downgrade the act to `follow_up`"));
}

#[test]
fn compiler_prompt_declares_exact_source_and_stateless_contract() {
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("Every source-bearing text field"));
    assert!(
        QUERY_COMPILER_SYSTEM_PROMPT.contains("surrounding-whitespace-free verbatim substring")
    );
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("When no prior conversation block"));
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("anaphora_unresolved"));
}

#[test]
fn compiler_prompt_keeps_configuration_artifact_requests_as_configure_how() {
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("artifact-shaped output"));
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("still `configure_how`"));
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("do not downgrade them to `describe`"));
    assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("`configuration_file` and `config_key`"));
}

#[test]
fn compiler_prompt_reserves_clarification_for_a_blocking_missing_choice() {
    assert!(
        QUERY_COMPILER_SYSTEM_PROMPT
            .contains("missing user choice makes a grounded answer impossible")
    );
    assert!(
        QUERY_COMPILER_SYSTEM_PROMPT
            .contains("broad `configure_how`, `describe`, or `enumerate` request")
    );
    assert!(
        QUERY_COMPILER_SYSTEM_PROMPT
            .contains("explain the evidence-derived variants in the answer")
    );
}
