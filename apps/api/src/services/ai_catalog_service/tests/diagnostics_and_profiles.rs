use super::*;

#[test]
fn credential_bearing_ai_catalog_commands_redact_diagnostics() {
    let api_key = "credential-must-not-appear";
    let base_url = "https://private.example.invalid/v1";
    let account_id = Uuid::now_v7();
    let provider_catalog_id = Uuid::now_v7();

    let create = CreateAiAccountCommand {
        scope_kind: AiScopeKind::Instance,
        workspace_id: None,
        library_id: None,
        provider_catalog_id,
        label: "operator account".to_string(),
        api_key: Some(api_key.to_string()),
        base_url: Some(base_url.to_string()),
        created_by_principal_id: None,
    };
    let update = UpdateAiAccountCommand {
        account_id,
        label: "operator account".to_string(),
        api_key: Some(api_key.to_string()),
        base_url: Some(base_url.to_string()),
        credential_state: "active".to_string(),
        expected_updated_at: chrono::Utc::now(),
    };
    let bootstrap_input = BootstrapAiCredentialInput {
        provider_kind: "provider-alpha".to_string(),
        api_key: Some(api_key.to_string()),
        base_url: Some(base_url.to_string()),
    };
    let bundle = ApplyBootstrapProviderBindingBundleCommand {
        provider_kind: "provider-alpha".to_string(),
        api_key: Some(api_key.to_string()),
        base_url: Some(base_url.to_string()),
        updated_by_principal_id: None,
    };

    for diagnostic in [
        format!("{create:?}"),
        format!("{update:?}"),
        format!("{bootstrap_input:?}"),
        format!("{bundle:?}"),
    ] {
        assert!(!diagnostic.contains(api_key));
        assert!(!diagnostic.contains(base_url));
        assert!(diagnostic.contains("<redacted>"));
    }
}

#[test]
fn resolved_runtime_binding_redacts_runtime_credentials_from_diagnostics() {
    let api_key = "runtime-credential-must-not-appear";
    let base_url = "https://private.example.invalid/v1";
    let binding = ResolvedRuntimeBinding {
        binding_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        binding_purpose: AiBindingPurpose::QueryAnswer,
        provider_catalog_id: Uuid::now_v7(),
        provider_kind: "provider-alpha".to_string(),
        provider_base_url: Some(base_url.to_string()),
        provider_api_style: "openai_compatible".to_string(),
        account_id: Uuid::now_v7(),
        api_key: Some(api_key.to_string()),
        model_catalog_id: Uuid::now_v7(),
        model_name: "sample-model".to_string(),
        effective_embedding_dimensions: None,
        system_prompt: Some("operator prompt".to_string()),
        temperature: Some(0.2),
        top_p: Some(0.8),
        max_output_tokens_override: Some(1_024),
        extra_parameters_json: serde_json::json!({}),
    };

    let diagnostic = format!("{binding:?}");

    assert!(!diagnostic.contains(api_key));
    assert!(!diagnostic.contains(base_url));
    assert!(diagnostic.contains("<redacted>"));
}

#[test]
fn provider_profile_rejects_legacy_boolean_capability_metadata() {
    let legacy_metadata = serde_json::json!({
        "chat": true,
        "embeddings": true,
        "vision": false
    });

    let result = serde_json::from_value::<ProviderProfile>(legacy_metadata);

    assert!(result.is_err(), "provider catalog rows must use the ProviderProfile shape");
}

#[test]
fn provider_profile_defaults_typed_policies_for_existing_metadata() {
    let provider = sample_provider("synthetic-router");
    let mut metadata =
        serde_json::to_value(&provider.profile).expect("provider profile should serialize");
    metadata.as_object_mut().expect("provider profile should be an object").remove("requestPolicy");
    metadata.as_object_mut().expect("provider profile should be an object").remove("usagePolicy");

    let parsed = serde_json::from_value::<ProviderProfile>(metadata)
        .expect("profiles created before requestPolicy should remain valid");

    assert_eq!(parsed.request_policy, ProviderRequestPolicy::default());
    assert_eq!(parsed.usage_policy, ProviderUsagePolicy::default());
}

#[test]
fn provider_profile_rejects_non_positive_request_policy_limit() {
    let provider = sample_provider("synthetic-router");
    let mut metadata =
        serde_json::to_value(&provider.profile).expect("provider profile should serialize");
    metadata["requestPolicy"]["defaultToolMaxOutputTokens"] = serde_json::json!(0);

    let error = parse_provider_profile(&provider.provider_kind, &metadata)
        .expect_err("non-positive provider request-policy limit must fail at the write boundary");

    assert!(matches!(error, ApiError::BadRequest(_)));
    assert!(format!("{error:?}").contains("defaultToolMaxOutputTokens must be positive"));
}

#[test]
fn provider_profile_rejects_malformed_bootstrap_presets_at_write_boundary() {
    let provider = sample_provider("synthetic-router");
    let base_metadata =
        serde_json::to_value(&provider.profile).expect("provider profile should serialize");
    let malformed_presets = [
        serde_json::json!({"purpose": "query_answer"}),
        serde_json::json!({"purpose": "query_answer", "modelName": "\t\n"}),
        serde_json::json!({
            "purpose": "query_answer",
            "modelName": "sample-model",
            "temperature": "cold"
        }),
        serde_json::json!({
            "purpose": "query_answer",
            "modelName": "sample-model",
            "topP": []
        }),
        serde_json::json!({
            "purpose": "query_answer",
            "modelName": "sample-model",
            "maxOutputTokensOverride": 1.5
        }),
        serde_json::json!({
            "purpose": "query_answer",
            "modelName": "sample-model",
            "systemPrompt": 42
        }),
        serde_json::json!({
            "purpose": "query_answer",
            "modelName": "sample-model",
            "extraParametersJson": []
        }),
        serde_json::json!({"purpose": "rerank", "modelName": "sample-model"}),
    ];

    for malformed_preset in malformed_presets {
        let mut metadata = base_metadata.clone();
        metadata["bootstrapPresets"] = serde_json::json!([malformed_preset]);
        let error = parse_provider_profile(&provider.provider_kind, &metadata)
            .expect_err("malformed bootstrap preset must fail at the provider write boundary");
        assert!(matches!(error, ApiError::BadRequest(_)));
    }

    let mut duplicate_metadata = base_metadata;
    duplicate_metadata["bootstrapPresets"] = serde_json::json!([
        {"purpose": "query_answer", "modelName": "sample-model-a"},
        {"purpose": "query_answer", "modelName": "sample-model-b"}
    ]);
    let duplicate_error = parse_provider_profile(&provider.provider_kind, &duplicate_metadata)
        .expect_err("duplicate bootstrap purposes must fail closed");
    assert!(matches!(duplicate_error, ApiError::BadRequest(_)));
}

#[test]
fn provider_profile_accepts_fully_typed_bootstrap_preset() {
    let provider = sample_provider("synthetic-router");
    let mut metadata =
        serde_json::to_value(&provider.profile).expect("provider profile should serialize");
    metadata["bootstrapPresets"] = serde_json::json!([{
        "purpose": "query_answer",
        "modelName": "sample-model",
        "systemPrompt": "Use supplied evidence.",
        "temperature": 0.2,
        "topP": 0.8,
        "maxOutputTokensOverride": 1024,
        "extraParametersJson": {"response_format": {"type": "json_object"}}
    }]);

    parse_provider_profile(&provider.provider_kind, &metadata)
        .expect("fully typed bootstrap preset should pass provider validation");
}

#[test]
fn runtime_profile_merge_overwrites_stale_preset_metadata() {
    let mut provider = sample_provider("synthetic-router");
    provider.profile.request_policy = ProviderRequestPolicy {
        sampling: ProviderSamplingPolicy::Omit,
        tool_choice: ProviderToolChoicePolicy::AutoOnly,
        default_tool_max_output_tokens: Some(2048),
        reasoning: crate::domains::provider_profiles::ProviderReasoningPolicy::Forward,
        ..ProviderRequestPolicy::default()
    };
    let stale_extra_parameters = serde_json::json!({
        "_providerProfile": {
            "runtime": {
                "kind": "stale_runtime",
                "authScheme": "raw_authorization",
                "tokenLimitParameter": "max_tokens",
                "chatPath": "/stale/chat",
                "embeddingsPath": "/stale/embeddings",
                "modelsPath": "/stale/models"
            }
        },
        "response_format": {"type": "json_object"}
    });

    let merged = merge_provider_runtime_profile(stale_extra_parameters, &provider.profile);

    assert_eq!(
        merged.pointer("/_providerProfile/runtime/kind").and_then(serde_json::Value::as_str),
        Some(OPENAI_COMPATIBLE_RUNTIME_KIND)
    );
    assert_eq!(
        merged.pointer("/_providerProfile/runtime/chatPath").and_then(serde_json::Value::as_str),
        Some("/chat/completions")
    );
    assert_eq!(
        merged.pointer("/response_format/type").and_then(serde_json::Value::as_str),
        Some("json_object")
    );
    assert_eq!(
        merged
            .pointer("/_providerProfile/requestPolicy/sampling")
            .and_then(serde_json::Value::as_str),
        Some("omit")
    );
    assert_eq!(
        merged
            .pointer("/_providerProfile/requestPolicy/toolChoice")
            .and_then(serde_json::Value::as_str),
        Some("auto_only")
    );
}

#[test]
fn model_request_policy_is_projected_into_internal_runtime_metadata() {
    let merged = merge_model_request_policy(
        serde_json::json!({"response_format": {"type": "json_object"}}),
        &serde_json::json!({
            "requestPolicy": {
                "sampling": "omit",
                "toolChoice": "auto_only",
                "defaultToolMaxOutputTokens": 4096
            }
        }),
    );

    assert_eq!(
        merged.pointer("/_providerRequestPolicy/sampling").and_then(serde_json::Value::as_str),
        Some("omit")
    );
    assert_eq!(
        merged.pointer("/_providerRequestPolicy/toolChoice").and_then(serde_json::Value::as_str),
        Some("auto_only")
    );
    assert_eq!(
        merged.pointer("/response_format/type").and_then(serde_json::Value::as_str),
        Some("json_object")
    );
}

#[test]
fn explicit_binding_request_policy_overrides_model_policy() {
    let merged = merge_model_request_policy(
        serde_json::json!({
            "_providerRequestPolicy": {
                "sampling": "forward",
                "toolChoice": "required_capable"
            }
        }),
        &serde_json::json!({
            "requestPolicy": {
                "sampling": "omit",
                "toolChoice": "auto_only"
            }
        }),
    );

    assert_eq!(
        merged.pointer("/_providerRequestPolicy/sampling").and_then(serde_json::Value::as_str),
        Some("forward")
    );
    assert_eq!(
        merged.pointer("/_providerRequestPolicy/toolChoice").and_then(serde_json::Value::as_str),
        Some("required_capable")
    );
}

#[test]
fn binding_request_policy_requires_valid_typed_metadata() {
    let invalid_variant = validate_binding_request_policy(&serde_json::json!({
        "_providerRequestPolicy": {"toolChoice": "guess_from_identity"}
    }));
    let invalid_limit = validate_binding_request_policy(&serde_json::json!({
        "_providerRequestPolicy": {"defaultToolMaxOutputTokens": 0}
    }));
    let valid = validate_binding_request_policy(&serde_json::json!({
        "_providerRequestPolicy": {
            "sampling": "omit",
            "toolChoice": "auto_only",
            "defaultToolMaxOutputTokens": 4096
        },
        "response_format": {"type": "json_object"}
    }));

    assert!(invalid_variant.is_err());
    assert!(invalid_limit.is_err());
    assert!(valid.is_ok());
}

#[test]
fn credential_validation_projects_selected_model_request_policy() {
    let provider = sample_provider("synthetic-router");
    let mut model = sample_model(vec![AiBindingPurpose::QueryAnswer]);
    model.metadata_json = serde_json::json!({
        "requestPolicy": {
            "sampling": "omit",
            "toolChoice": "auto_only",
            "defaultToolMaxOutputTokens": 2048
        }
    });

    let extra_parameters = provider_validation_extra_parameters(&provider, &model);

    assert_eq!(
        extra_parameters
            .pointer("/_providerRequestPolicy/sampling")
            .and_then(serde_json::Value::as_str),
        Some("omit")
    );
    assert_eq!(
        extra_parameters
            .pointer("/_providerProfile/runtime/kind")
            .and_then(serde_json::Value::as_str),
        Some(OPENAI_COMPATIBLE_RUNTIME_KIND)
    );
}

#[test]
fn model_metadata_rejects_invalid_typed_request_policy() {
    let invalid_variant = validate_model_metadata(serde_json::json!({
        "requestPolicy": {"sampling": "guess_from_identity"}
    }));
    let invalid_limit = validate_model_metadata(serde_json::json!({
        "requestPolicy": {
            "sampling": "omit",
            "defaultToolMaxOutputTokens": 0
        }
    }));

    assert!(invalid_variant.is_err());
    assert!(invalid_limit.is_err());
}

#[test]
fn model_metadata_accepts_explicit_typed_request_policy() {
    let metadata = validate_model_metadata(serde_json::json!({
        "requestPolicy": {
            "sampling": "omit",
            "toolChoice": "auto_only",
            "defaultToolMaxOutputTokens": 4096
        }
    }))
    .expect("typed request policy should be valid");

    assert_eq!(
        metadata.pointer("/requestPolicy/toolChoice").and_then(serde_json::Value::as_str),
        Some("auto_only")
    );
}
