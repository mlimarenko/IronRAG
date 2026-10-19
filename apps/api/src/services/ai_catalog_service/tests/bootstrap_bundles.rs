use super::*;

#[test]
fn configured_bootstrap_replay_preserves_operator_tuning_for_the_same_target() {
    let account_id = Uuid::from_u128(2);
    let model_id = Uuid::from_u128(3);
    let mut existing = sample_bootstrap_binding(account_id, model_id);
    let input = sample_bootstrap_binding_input(model_id);

    assert!(bootstrap_binding_update_command(&existing, &input, account_id, None).is_none());

    existing.binding_state = "disabled".to_string();
    let command = bootstrap_binding_update_command(&existing, &input, account_id, None)
        .expect("inactive binding must be reactivated");
    assert_eq!(command.system_prompt, existing.system_prompt);
    assert_eq!(command.temperature, existing.temperature);
    assert_eq!(command.top_p, existing.top_p);
    assert_eq!(command.max_output_tokens_override, existing.max_output_tokens_override);
    assert_eq!(command.extra_parameters_json, existing.extra_parameters_json);
    assert_eq!(command.binding_state, "active");
}

#[test]
fn configured_bootstrap_target_change_adopts_the_new_complete_preset() {
    let existing = sample_bootstrap_binding(Uuid::from_u128(4), Uuid::from_u128(5));
    let new_account_id = Uuid::from_u128(6);
    let new_model_id = Uuid::from_u128(7);
    let input = sample_bootstrap_binding_input(new_model_id);

    let command = bootstrap_binding_update_command(&existing, &input, new_account_id, None)
        .expect("target change must update the binding");
    assert_eq!(command.account_id, new_account_id);
    assert_eq!(command.model_catalog_id, new_model_id);
    assert_eq!(command.system_prompt, input.system_prompt);
    assert_eq!(command.temperature, input.temperature);
    assert_eq!(command.top_p, input.top_p);
    assert_eq!(command.max_output_tokens_override, input.max_output_tokens_override);
    assert_eq!(command.extra_parameters_json, input.extra_parameters_json);
}

#[test]
fn bootstrap_bundle_uses_expected_provider_alpha_models() {
    let provider = sample_provider("provider-alpha");
    let extract_graph_model_id = Uuid::now_v7();
    let query_answer_model_id = Uuid::now_v7();
    let embed_model_id = Uuid::now_v7();
    let models = vec![
        catalog_model(
            extract_graph_model_id,
            provider.id,
            "alpha-chat-mini",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
        catalog_model(
            query_answer_model_id,
            provider.id,
            "alpha-chat-plus",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
        catalog_model(
            embed_model_id,
            provider.id,
            "alpha-embedding-large",
            vec![AiBindingPurpose::EmbedChunk],
        ),
    ];

    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("provider-alpha bundle should resolve")
    .expect("provider-alpha bundle should be available");

    assert_eq!(bundle.provider_kind, "provider-alpha");
    assert_eq!(bundle.bindings.len(), 6);
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractText)
            .map(|binding| binding.model_name.as_str()),
        Some("alpha-chat-plus")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractGraph)
            .map(|binding| binding.model_name.as_str()),
        Some("alpha-chat-mini")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::QueryCompile)
            .map(|binding| binding.model_name.as_str()),
        Some("alpha-chat-plus")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::QueryAnswer)
            .and_then(|binding| binding.temperature),
        Some(0.0)
    );
}

#[test]
fn bootstrap_binding_descriptors_preserve_typed_provider_settings() {
    let provider = sample_provider("provider-zeta");
    let model = catalog_model(
        Uuid::now_v7(),
        provider.id,
        "zeta-chat",
        vec![AiBindingPurpose::ExtractGraph],
    );

    let descriptors = resolve_bootstrap_provider_binding_descriptors(
        &provider,
        std::slice::from_ref(&provider),
        &[model],
    )
    .expect("provider bootstrap bindings should parse");

    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].binding_purpose, AiBindingPurpose::ExtractGraph);
    assert_eq!(descriptors[0].system_prompt.as_deref(), Some("Extract only supported facts."));
    assert_eq!(
        descriptors[0].extra_parameters_json,
        serde_json::json!({"sample": {"enabled": true}})
    );
}

#[test]
fn bootstrap_bundle_uses_expected_provider_gamma_models() {
    let provider = sample_provider("provider-gamma");
    let graph_model_id = Uuid::now_v7();
    let runtime_model_id = Uuid::now_v7();
    let embed_model_id = Uuid::now_v7();
    let vision_model_id = Uuid::now_v7();
    let models = vec![
        catalog_model(
            graph_model_id,
            provider.id,
            "provider-gamma-chat-flash",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
        catalog_model(
            runtime_model_id,
            provider.id,
            "gamma-chat-max",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
        catalog_model(
            embed_model_id,
            provider.id,
            "gamma-embedding-large",
            vec![AiBindingPurpose::EmbedChunk],
        ),
        catalog_model(
            vision_model_id,
            provider.id,
            "provider-gamma-vl-max",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
    ];

    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("provider-gamma bundle should resolve")
    .expect("provider-gamma bundle should be available");

    assert_eq!(bundle.provider_kind, "provider-gamma");
    assert_eq!(bundle.bindings.len(), 6);
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractText)
            .map(|binding| binding.model_name.as_str()),
        Some("provider-gamma-vl-max")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::QueryCompile)
            .map(|binding| binding.model_name.as_str()),
        Some("gamma-chat-max")
    );
}

#[test]
fn bootstrap_binding_descriptors_keep_partial_provider_bindings() {
    let provider = sample_provider("provider-delta");
    let models = vec![catalog_model(
        Uuid::now_v7(),
        provider.id,
        "provider-delta-chat",
        vec![
            AiBindingPurpose::ExtractText,
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
            AiBindingPurpose::Agent,
        ],
    )];

    let descriptors = resolve_bootstrap_provider_binding_descriptors(
        &provider,
        std::slice::from_ref(&provider),
        &models,
    )
    .expect("provider-delta binding descriptors should resolve");
    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("provider-delta bundle resolution should not fail");

    assert_eq!(descriptors.len(), 4);
    assert!(
        !descriptors.iter().any(|binding| binding.binding_purpose == AiBindingPurpose::ExtractText)
    );
    assert!(descriptors.iter().any(|binding| {
        binding.binding_purpose == AiBindingPurpose::QueryCompile
            && binding.model_name == "provider-delta-chat"
    }));
    assert!(bundle.is_none());
}

#[test]
fn bootstrap_bundle_uses_expected_provider_beta_models() {
    let mut provider = sample_provider("provider-beta");
    provider.capabilities.tools = ProviderCapabilityState::Supported;
    let graph_model_id = Uuid::now_v7();
    let embed_model_id = Uuid::now_v7();
    let vision_model_id = Uuid::now_v7();
    let models = vec![
        catalog_model(
            graph_model_id,
            provider.id,
            "beta-chat-small",
            vec![
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
        catalog_model(
            embed_model_id,
            provider.id,
            "beta-embedding-small",
            vec![AiBindingPurpose::EmbedChunk],
        ),
        catalog_model(
            vision_model_id,
            provider.id,
            "beta-chat-vision",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
    ];

    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("provider-beta bundle should resolve")
    .expect("provider-beta bundle should be available");

    assert_eq!(bundle.provider_kind, "provider-beta");
    assert_eq!(bundle.default_base_url.as_deref(), Some("http://localhost:11434/v1"));
    assert!(!bundle.api_key_required);
    assert!(bundle.base_url_required);
    assert_eq!(bundle.bindings.len(), 6);
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractText)
            .map(|binding| binding.model_name.as_str()),
        Some("beta-chat-vision")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractGraph)
            .map(|binding| binding.model_name.as_str()),
        Some("beta-chat-small")
    );
}

#[test]
fn bootstrap_bundle_uses_expected_provider_epsilon_models() {
    let provider = sample_provider("provider-epsilon");
    let graph_model_id = Uuid::now_v7();
    let answer_model_id = Uuid::now_v7();
    let embed_model_id = Uuid::now_v7();
    let models = vec![
        catalog_model(
            graph_model_id,
            provider.id,
            "provider-omega/chat-mini",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
        catalog_model(
            answer_model_id,
            provider.id,
            "provider-omega/chat-vision",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
        catalog_model(
            embed_model_id,
            provider.id,
            "provider-omega/alpha-embedding-small",
            vec![AiBindingPurpose::EmbedChunk],
        ),
    ];

    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("provider-epsilon bundle should resolve")
    .expect("provider-epsilon bundle should be available");

    assert_eq!(bundle.provider_kind, "provider-epsilon");
    assert_eq!(bundle.bindings.len(), 6);
    assert!(bootstrap_bundle_is_self_contained(&bundle));
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractText)
            .map(|binding| binding.model_name.as_str()),
        Some("provider-omega/chat-vision")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::EmbedChunk)
            .map(|binding| binding.model_name.as_str()),
        Some("provider-omega/alpha-embedding-small")
    );
}

#[test]
fn bootstrap_model_list_bindings_require_provider_discovered_models() {
    let provider = sample_provider("provider-beta");
    let graph_model_id = Uuid::now_v7();
    let embed_model_id = Uuid::now_v7();
    let models = vec![
        catalog_model(
            graph_model_id,
            provider.id,
            "beta-chat-small",
            vec![AiBindingPurpose::ExtractGraph],
        ),
        catalog_model(
            embed_model_id,
            provider.id,
            "beta-embedding-small",
            vec![AiBindingPurpose::EmbedChunk],
        ),
    ];
    let binding_inputs = vec![
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::ExtractGraph,
            provider_kind: provider.provider_kind.clone(),
            model_catalog_id: graph_model_id,
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::EmbedChunk,
            provider_kind: provider.provider_kind.clone(),
            model_catalog_id: embed_model_id,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
    ];

    let missing = missing_bootstrap_model_list_models(
        &provider,
        &binding_inputs,
        &models,
        &["beta-chat-small".to_string()],
    )
    .expect("model-list validation should compare selected catalog models");

    assert_eq!(missing, vec!["beta-embedding-small"]);
}

#[test]
fn configured_bootstrap_bindings_inherit_provider_bundle_tuning_when_models_match() {
    let provider = sample_provider("provider-alpha");
    let model = catalog_model(
        Uuid::now_v7(),
        provider.id,
        "alpha-chat-mini",
        vec![AiBindingPurpose::ExtractGraph],
    );
    let configured = crate::app::config::UiBootstrapAiSetup {
        provider_secrets: vec![crate::app::config::UiBootstrapAiProviderSecret {
            provider_kind: "provider-alpha".to_string(),
            api_key: "test-provider-alpha-key".to_string(), // pragma: allowlist secret
        }],
        binding_defaults: vec![UiBootstrapAiBindingDefault {
            binding_purpose: AiBindingPurpose::ExtractGraph,
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-mini".to_string()),
        }],
    };

    let binding_inputs = resolve_configured_bootstrap_binding_inputs(
        &configured,
        std::slice::from_ref(&provider),
        &[model],
    )
    .expect("configured binding inputs should resolve");

    assert_eq!(binding_inputs.len(), 1);
    assert_eq!(binding_inputs[0].provider_kind, "provider-alpha");
    assert_eq!(binding_inputs[0].binding_purpose, AiBindingPurpose::ExtractGraph);
    assert_eq!(binding_inputs[0].temperature, Some(0.0));
    assert_eq!(binding_inputs[0].top_p, Some(1.0));
}

#[test]
fn bootstrap_bundle_omits_incomplete_provider_profiles() {
    let provider_delta = sample_provider("provider-delta");
    let models = vec![catalog_model(
        Uuid::now_v7(),
        provider_delta.id,
        "provider-delta-chat",
        vec![
            AiBindingPurpose::ExtractText,
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
        ],
    )];

    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider_delta,
        std::slice::from_ref(&provider_delta),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("provider-delta resolution should not error");

    assert!(bundle.is_none());
}

#[test]
fn provider_bootstrap_bundle_never_borrows_models_from_another_provider() {
    let provider_alpha_provider = sample_provider("provider-alpha");
    let provider_delta = sample_provider("provider-delta");
    let providers = vec![provider_delta.clone(), provider_alpha_provider.clone()];
    let models = vec![
        catalog_model(
            Uuid::now_v7(),
            provider_delta.id,
            "provider-delta-chat",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
        catalog_model(
            Uuid::now_v7(),
            provider_alpha_provider.id,
            "alpha-embedding-large",
            vec![AiBindingPurpose::EmbedChunk],
        ),
        catalog_model(
            Uuid::now_v7(),
            provider_alpha_provider.id,
            "alpha-chat-plus",
            vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
            ],
        ),
    ];

    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider_delta,
        &providers,
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("provider-delta resolution should not error");

    assert!(bundle.is_none());
}

#[test]
fn bootstrap_bundle_requires_document_understanding() {
    let provider = sample_provider("provider-alpha");
    let models = vec![
        catalog_model(
            Uuid::now_v7(),
            provider.id,
            "alpha-chat-mini",
            vec![AiBindingPurpose::ExtractGraph],
        ),
        catalog_model(
            Uuid::now_v7(),
            provider.id,
            "alpha-embedding-large",
            vec![AiBindingPurpose::EmbedChunk],
        ),
        catalog_model(
            Uuid::now_v7(),
            provider.id,
            "alpha-chat-plus",
            vec![
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
        ),
    ];
    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("bundle resolution should not fail");

    assert!(bundle.is_none());
}
