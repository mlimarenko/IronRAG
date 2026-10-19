use super::*;

#[test]
fn model_catalog_row_uses_typed_relation_as_eligibility_authority() {
    let row = sample_model_row(
        "active",
        serde_json::json!({
            "requestPolicy": {"sampling": "omit"}
        }),
    );

    let mapped = map_model_row(row).expect("typed relation values must map");

    assert_eq!(
        mapped.allowed_binding_purposes,
        vec![AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer]
    );
    assert!(mapped.metadata_json.get("defaultRoles").is_none());
    assert_eq!(mapped.metadata_json["requestPolicy"]["sampling"], "omit");
}

#[test]
fn model_metadata_rejects_legacy_default_roles_authority() {
    let error = validate_model_metadata(serde_json::json!({
        "defaultRoles": ["query_answer"]
    }))
    .expect_err("legacy metadata authority must be rejected");

    assert!(matches!(error, ApiError::BadRequest(_)));
    assert!(format!("{error:?}").contains("allowedBindingPurposes"));
}

#[test]
fn resolved_catalog_uses_persisted_active_accounts_without_remote_discovery() {
    let provider_id = Uuid::from_u128(10);
    let account_id = Uuid::from_u128(11);
    let model = ModelCatalogEntry {
        provider_catalog_id: provider_id,
        model_name: "known-model".to_string(),
        ..sample_model(vec![AiBindingPurpose::QueryAnswer])
    };

    let account_ids = model_available_account_ids(
        &model,
        &HashMap::from([(provider_id, BTreeSet::from([account_id]))]),
    );

    assert_eq!(account_ids, [account_id]);
}

#[test]
fn model_update_request_rejects_immutable_identity_fields() {
    let payloads = [
        serde_json::json!({
            "providerCatalogId": Uuid::now_v7(),
            "lifecycleState": "active",
            "allowedBindingPurposes": ["agent"],
            "contextWindow": null,
            "maxOutputTokens": null,
            "metadataJson": null
        }),
        serde_json::json!({
            "modelName": "replacement-model",
            "lifecycleState": "active",
            "allowedBindingPurposes": ["agent"],
            "contextWindow": null,
            "maxOutputTokens": null,
            "metadataJson": null
        }),
    ];

    for payload in payloads {
        let result = serde_json::from_value::<crate::interfaces::http::ai::UpdateModelCatalogRequest>(
            payload,
        );
        assert!(result.is_err(), "model identity must be rejected, not ignored");
    }
}

#[test]
fn rejects_incompatible_binding_purpose() {
    let model = sample_model(vec![AiBindingPurpose::EmbedChunk]);
    let error = validate_model_binding_purpose(AiBindingPurpose::ExtractGraph, &model)
        .expect_err("incompatible purpose should fail");
    assert!(matches!(error, ApiError::BadRequest(_)));
    assert!(format!("{error:?}").contains("incompatible"));
}

#[test]
fn typed_model_purpose_relation_is_the_only_eligibility_authority() {
    let document_model = sample_model(vec![AiBindingPurpose::ExtractText]);
    validate_model_binding_purpose(AiBindingPurpose::ExtractText, &document_model)
        .expect("the typed relation authorizes document extraction without modality heuristics");

    let agent_model = sample_model(vec![AiBindingPurpose::Agent]);
    validate_model_binding_purpose(AiBindingPurpose::Agent, &agent_model)
        .expect("the typed relation authorizes agent use without capability heuristics");
}

#[test]
fn query_understanding_accepts_query_compile_only_models() {
    let model = sample_model(vec![AiBindingPurpose::QueryCompile]);
    validate_model_binding_purpose(AiBindingPurpose::QueryCompile, &model)
        .expect("the typed relation authorizes the compiler purpose");
}

#[test]
fn semantic_rerank_requires_its_own_typed_model_role() {
    let compiler_only = sample_model(vec![AiBindingPurpose::QueryCompile]);
    let error = validate_model_binding_purpose(AiBindingPurpose::QueryRerank, &compiler_only)
        .expect_err("a compiler-only model must not silently serve rerank");
    assert!(format!("{error:?}").contains("incompatible"));

    let rerank_model = sample_model(vec![AiBindingPurpose::QueryRerank]);
    validate_model_binding_purpose(AiBindingPurpose::QueryRerank, &rerank_model)
        .expect("the typed relation authorizes the rerank purpose");
}

#[test]
fn query_rerank_is_not_a_required_bootstrap_purpose() {
    assert_eq!(REQUIRED_BINDING_PURPOSES.len(), 6, "the required bootstrap set stays at six");
    assert!(
        !REQUIRED_BINDING_PURPOSES.contains(&AiBindingPurpose::QueryRerank),
        "query_rerank binds independently and must not gate bootstrap",
    );
}

#[test]
fn bootstrap_accepts_an_optional_query_rerank_binding() {
    let without_rerank = REQUIRED_BINDING_PURPOSES
        .into_iter()
        .map(bootstrap_binding_input_for_purpose)
        .collect::<Vec<_>>();
    assert!(bootstrap_binding_inputs_cover_required_purposes(&without_rerank));

    let mut with_rerank = without_rerank;
    with_rerank.push(bootstrap_binding_input_for_purpose(AiBindingPurpose::QueryRerank));
    assert!(bootstrap_binding_inputs_cover_required_purposes(&with_rerank));
    validate_bootstrap_binding_inputs_cover_required_purposes(&with_rerank)
        .expect("an optional query_rerank binding must not fail bootstrap");
}

#[test]
fn bootstrap_binding_inputs_cover_all_six_required_purposes() {
    let inputs = REQUIRED_BINDING_PURPOSES
        .into_iter()
        .map(bootstrap_binding_input_for_purpose)
        .collect::<Vec<_>>();

    assert!(bootstrap_binding_inputs_cover_required_purposes(&inputs));
    validate_bootstrap_binding_inputs_cover_required_purposes(&inputs)
        .expect("all six required purposes are configured");
}

#[test]
fn bootstrap_binding_inputs_reject_missing_explicit_agent_purpose() {
    let inputs = REQUIRED_BINDING_PURPOSES
        .into_iter()
        .filter(|purpose| *purpose != AiBindingPurpose::Agent)
        .map(bootstrap_binding_input_for_purpose)
        .collect::<Vec<_>>();

    assert!(!bootstrap_binding_inputs_cover_required_purposes(&inputs));
    assert!(matches!(
        validate_bootstrap_binding_inputs_cover_required_purposes(&inputs),
        Err(ApiError::BadRequest(_))
    ));
}

#[test]
fn query_answer_bootstrap_preset_never_substitutes_agent() {
    let mut provider = sample_provider("provider-alpha");
    provider
        .capability_flags_json
        .get_mut("bootstrapPresets")
        .and_then(serde_json::Value::as_array_mut)
        .expect("sample provider must expose bootstrap presets")
        .retain(|preset| {
            preset.get("purpose").and_then(serde_json::Value::as_str) != Some("agent")
        });

    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &[],
        BootstrapAiCredentialSource::Missing,
    )
    .expect("profile validation should not fail");

    assert!(bundle.is_none(), "query_answer must not synthesize an agent binding");
}

#[test]
fn bootstrap_agent_preset_uses_typed_model_purpose_when_provider_tools_are_unknown() {
    let provider = sample_provider("provider-beta");
    let model = catalog_model(
        Uuid::now_v7(),
        provider.id,
        "beta-chat-small",
        vec![AiBindingPurpose::Agent],
    );

    let descriptors = resolve_bootstrap_provider_binding_descriptors(
        &provider,
        std::slice::from_ref(&provider),
        &[model],
    )
    .expect("typed bootstrap profile should still parse");

    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].binding_purpose, AiBindingPurpose::Agent);
}

#[test]
fn missing_agent_preset_prevents_implicit_agent_model_suggestion() {
    let mut provider = sample_provider("provider-alpha");
    provider
        .capability_flags_json
        .get_mut("bootstrapPresets")
        .and_then(serde_json::Value::as_array_mut)
        .expect("sample provider must expose bootstrap presets")
        .retain(|preset| {
            preset.get("purpose").and_then(serde_json::Value::as_str) != Some("agent")
        });
    let model = catalog_model(
        Uuid::now_v7(),
        provider.id,
        "agent-capable-model",
        vec![AiBindingPurpose::Agent, AiBindingPurpose::QueryAnswer],
    );
    let configured = crate::app::config::UiBootstrapAiSetup {
        provider_secrets: vec![crate::app::config::UiBootstrapAiProviderSecret {
            provider_kind: provider.provider_kind.clone(),
            api_key: "test-provider-alpha-key".to_string(), // pragma: allowlist secret
        }],
        binding_defaults: Vec::new(),
    };

    let selections = resolve_configured_bootstrap_binding_inputs(
        &configured,
        std::slice::from_ref(&provider),
        &[model],
    )
    .expect("missing defaults should resolve without synthesizing agent");

    assert!(
        selections.iter().all(|selection| selection.binding_purpose != AiBindingPurpose::Agent),
        "an agent-capable model is not an implicit Agent preset"
    );
}

#[test]
fn bootstrap_preset_purpose_rejects_whitespace_aliases() {
    let mut provider = sample_provider("provider-alpha");
    let agent_preset = provider
        .capability_flags_json
        .get_mut("bootstrapPresets")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|presets| {
            presets.iter_mut().find(|preset| {
                preset.get("purpose").and_then(serde_json::Value::as_str) == Some("agent")
            })
        })
        .expect("sample provider must expose an agent preset");
    agent_preset["purpose"] = serde_json::Value::String(" agent".to_string());

    let error = resolve_bootstrap_provider_binding_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &[],
        BootstrapAiCredentialSource::Missing,
    )
    .expect_err("binding purpose aliases must fail closed");

    assert!(matches!(error, ApiError::Internal));
}
