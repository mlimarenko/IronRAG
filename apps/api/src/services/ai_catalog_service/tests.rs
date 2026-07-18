use super::bootstrap::{bootstrap_binding_update_command, missing_bootstrap_model_list_models};
use super::catalog::parse_provider_profile;
use super::provider_validation::{
    ChatRoundTripValidationFailureKind, ProviderCredentialValidationFailureKind,
    chat_round_trip_validation_failure_kind, normalize_provider_base_url_input,
    provider_credential_validation_error, provider_validation_extra_parameters,
};
use super::{
    BootstrapAiBindingInput, BootstrapAiCredentialSource,
    bootstrap_binding_inputs_cover_required_purposes, bootstrap_bundle_is_self_contained,
    bootstrap_env_credential_needs_sync, canonicalize_provider_base_url,
    deduplicate_binding_purposes, discovered_provider_model_signature_for_capability,
    is_loopback_base_url, is_provider_credential_validation_error, map_model_row,
    merge_model_request_policy, merge_provider_runtime_profile, metadata_with_binding_purposes,
    parse_allowed_binding_purposes, provider_credential_base_url_for_create,
    provider_credential_base_url_for_update, resolve_bootstrap_provider_binding_bundle,
    resolve_bootstrap_provider_binding_descriptors, resolve_configured_bootstrap_binding_inputs,
    resolve_effective_embedding_dimensions, runtime_provider_base_url,
    validate_binding_request_policy, validate_bootstrap_binding_inputs_cover_required_purposes,
    validate_model_binding_purpose, validate_provider_base_url_key_reuse,
    validate_provider_capability_for_binding,
};
use crate::app::config::UiBootstrapAiBindingDefault;
use crate::domains::ai::{
    AiBinding, AiBindingPurpose, AiScopeKind, ModelCatalogEntry, ProviderCatalogEntry,
};
use crate::domains::provider_profiles::{
    OPENAI_COMPATIBLE_RUNTIME_KIND, ProviderAuthScheme, ProviderBaseUrlMode, ProviderBaseUrlPolicy,
    ProviderCapabilities, ProviderCapabilityState, ProviderCredentialPolicy,
    ProviderCredentialValidationMode, ProviderModelDiscovery, ProviderModelDiscoveryMode,
    ProviderModelDiscoveryPath, ProviderProfile, ProviderRequestPolicy, ProviderRuntimeProfile,
    ProviderSamplingPolicy, ProviderStructuredOutputMode, ProviderTokenLimitParameter,
    ProviderToolChoicePolicy, ProviderUsagePolicy,
};
use crate::interfaces::http::router_support::ApiError;
use crate::shared::secret_encryption::{CredentialCipher, SecretPurpose};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use uuid::Uuid;

fn sample_model(allowed_binding_purposes: Vec<AiBindingPurpose>) -> ModelCatalogEntry {
    ModelCatalogEntry {
        id: Uuid::nil(),
        provider_catalog_id: Uuid::nil(),
        model_name: "sample-model".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "text".to_string(),
        lifecycle_state: "active".to_string(),
        metadata_json: serde_json::json!({}),
        allowed_binding_purposes,
        context_window: None,
        max_output_tokens: None,
    }
}

fn sample_bootstrap_binding(account_id: Uuid, model_catalog_id: Uuid) -> AiBinding {
    AiBinding {
        id: Uuid::from_u128(1),
        scope_kind: AiScopeKind::Instance,
        workspace_id: None,
        library_id: None,
        binding_purpose: AiBindingPurpose::EmbedChunk,
        account_id,
        model_catalog_id,
        system_prompt: Some("operator prompt".to_string()),
        temperature: Some(0.11),
        top_p: Some(0.22),
        max_output_tokens_override: Some(9_000),
        extra_parameters_json: serde_json::json!({
            "dimensions": 3072,
            "encoding_format": "float"
        }),
        binding_state: "active".to_string(),
    }
}

fn sample_bootstrap_binding_input(model_catalog_id: Uuid) -> BootstrapAiBindingInput {
    BootstrapAiBindingInput {
        binding_purpose: AiBindingPurpose::EmbedChunk,
        provider_kind: "provider-alpha".to_string(),
        model_catalog_id,
        system_prompt: Some("provider preset".to_string()),
        temperature: Some(0.33),
        top_p: Some(0.44),
        max_output_tokens_override: Some(1_000),
        extra_parameters_json: serde_json::json!({"encoding_format": "base64"}),
    }
}

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

fn sample_model_row(
    lifecycle_state: &str,
    metadata_json: serde_json::Value,
) -> crate::infra::repositories::ai_repository::AiModelCatalogRow {
    crate::infra::repositories::ai_repository::AiModelCatalogRow {
        id: Uuid::nil(),
        provider_catalog_id: Uuid::nil(),
        model_name: "sample-model".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "text".to_string(),
        context_window: None,
        max_output_tokens: None,
        lifecycle_state: lifecycle_state.to_string(),
        metadata_json,
    }
}

fn sample_provider(provider_kind: &str) -> ProviderCatalogEntry {
    let is_local_provider = provider_kind == "provider-beta";
    let credential_policy = ProviderCredentialPolicy {
        api_key_required: !is_local_provider,
        base_url_required: is_local_provider,
        base_url_mode: if is_local_provider {
            ProviderBaseUrlMode::Required
        } else {
            ProviderBaseUrlMode::Fixed
        },
        validation_mode: if is_local_provider {
            ProviderCredentialValidationMode::ModelList
        } else {
            ProviderCredentialValidationMode::ChatRoundTrip
        },
    };
    let base_url_policy = ProviderBaseUrlPolicy {
        allow_override: is_local_provider,
        require_https: !is_local_provider,
        allow_private_network: is_local_provider,
        trim_suffixes: Vec::new(),
    };
    let model_discovery = ProviderModelDiscovery {
        mode: ProviderModelDiscoveryMode::Credential,
        paths: vec![
            ProviderModelDiscoveryPath {
                capability_kind: "chat".to_string(),
                path: "/models".to_string(),
            },
            ProviderModelDiscoveryPath {
                capability_kind: "embedding".to_string(),
                path: "/models".to_string(),
            },
        ],
    };
    let capabilities = ProviderCapabilities {
        chat: ProviderCapabilityState::Supported,
        embeddings: ProviderCapabilityState::Supported,
        vision: ProviderCapabilityState::Supported,
        streaming: ProviderCapabilityState::Supported,
        tools: if is_local_provider {
            ProviderCapabilityState::Unknown
        } else {
            ProviderCapabilityState::Supported
        },
        model_discovery: ProviderCapabilityState::Supported,
    };
    let runtime = ProviderRuntimeProfile {
        kind: OPENAI_COMPATIBLE_RUNTIME_KIND.to_string(),
        auth_scheme: ProviderAuthScheme::Bearer,
        token_limit_parameter: if provider_kind == "provider-alpha" {
            ProviderTokenLimitParameter::MaxCompletionTokens
        } else {
            ProviderTokenLimitParameter::MaxTokens
        },
        structured_output: ProviderStructuredOutputMode::JsonSchema,
        chat_path: "/chat/completions".to_string(),
        embeddings_path: Some("/embeddings".to_string()),
        models_path: Some("/models".to_string()),
    };
    let capability_flags_json = bootstrap_capability_flags(provider_kind);
    let ui_hints =
        capability_flags_json.get("uiHints").cloned().unwrap_or_else(|| serde_json::json!({}));
    let profile = ProviderProfile {
        runtime: runtime.clone(),
        credentials: credential_policy.clone(),
        base_url: base_url_policy.clone(),
        model_discovery: model_discovery.clone(),
        capabilities: capabilities.clone(),
        request_policy: ProviderRequestPolicy::default(),
        usage_policy: ProviderUsagePolicy::default(),
        ui_hints: ui_hints.clone(),
    };
    ProviderCatalogEntry {
        id: Uuid::now_v7(),
        provider_kind: provider_kind.to_string(),
        display_name: provider_kind.to_string(),
        api_style: "openai_compatible".to_string(),
        lifecycle_state: "active".to_string(),
        default_base_url: Some(if is_local_provider {
            "http://localhost:11434/v1".to_string()
        } else {
            "https://example.com/v1".to_string()
        }),
        capability_flags_json,
        api_key_required: credential_policy.api_key_required,
        base_url_required: credential_policy.base_url_required,
        credential_policy,
        base_url_policy,
        model_discovery,
        capabilities,
        runtime,
        ui_hints,
        profile,
    }
}

fn signature_for_capability(
    provider: &ProviderCatalogEntry,
    capability_kind: &str,
) -> super::provider_validation::DiscoveredModelSignature {
    discovered_provider_model_signature_for_capability(provider, capability_kind)
        .expect("capability kind should be valid")
        .expect("signature expected")
}

#[test]
fn provider_profile_rejects_legacy_boolean_capability_metadata() {
    let legacy_metadata = serde_json::json!({
        "chat": true,
        "embeddings": true,
        "vision": false
    });

    let result = serde_json::from_value::<ProviderProfile>(legacy_metadata);

    assert!(result.is_err(), "provider catalog rows must use the canonical ProviderProfile shape");
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
    let invalid_variant = metadata_with_binding_purposes(
        serde_json::json!({"requestPolicy": {"sampling": "guess_from_identity"}}),
        &[AiBindingPurpose::QueryAnswer],
    );
    let invalid_limit = metadata_with_binding_purposes(
        serde_json::json!({
            "requestPolicy": {
                "sampling": "omit",
                "defaultToolMaxOutputTokens": 0
            }
        }),
        &[AiBindingPurpose::Agent],
    );

    assert!(invalid_variant.is_err());
    assert!(invalid_limit.is_err());
}

#[test]
fn model_metadata_accepts_explicit_typed_request_policy() {
    let metadata = metadata_with_binding_purposes(
        serde_json::json!({
            "requestPolicy": {
                "sampling": "omit",
                "toolChoice": "auto_only",
                "defaultToolMaxOutputTokens": 4096
            }
        }),
        &[AiBindingPurpose::Agent],
    )
    .expect("typed request policy should be valid");

    assert_eq!(
        metadata.pointer("/requestPolicy/toolChoice").and_then(serde_json::Value::as_str),
        Some("auto_only")
    );
}

fn bootstrap_capability_flags(provider_kind: &str) -> serde_json::Value {
    match provider_kind {
        "provider-alpha" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_text", "modelName": "alpha-chat-plus"},
                {"purpose": "extract_graph", "modelName": "alpha-chat-mini"},
                {"purpose": "embed_chunk", "modelName": "alpha-embedding-large"},
                {"purpose": "query_compile", "modelName": "alpha-chat-plus"},
                {"purpose": "query_answer", "modelName": "alpha-chat-plus"},
                {"purpose": "agent", "modelName": "alpha-chat-plus"}
            ],
            "uiHints": {"accent": "neutral"}
        }),
        "provider-beta" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_text", "modelName": "beta-chat-vision"},
                {"purpose": "extract_graph", "modelName": "beta-chat-small"},
                {"purpose": "embed_chunk", "modelName": "beta-embedding-small"},
                {"purpose": "query_compile", "modelName": "beta-chat-small"},
                {"purpose": "query_answer", "modelName": "beta-chat-small"},
                {"purpose": "agent", "modelName": "beta-chat-small"}
            ]
        }),
        "provider-gamma" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_text", "modelName": "provider-gamma-vl-max"},
                {"purpose": "extract_graph", "modelName": "provider-gamma-chat-flash"},
                {"purpose": "embed_chunk", "modelName": "gamma-embedding-large"},
                {"purpose": "query_compile", "modelName": "gamma-chat-max"},
                {"purpose": "query_answer", "modelName": "gamma-chat-max"},
                {"purpose": "agent", "modelName": "gamma-chat-max"}
            ]
        }),
        "provider-delta" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_graph", "modelName": "provider-delta-chat"},
                {"purpose": "query_compile", "modelName": "provider-delta-chat"},
                {"purpose": "query_answer", "modelName": "provider-delta-chat"},
                {"purpose": "agent", "modelName": "provider-delta-chat"}
            ]
        }),
        "provider-epsilon" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_text", "modelName": "provider-omega/chat-vision"},
                {"purpose": "extract_graph", "modelName": "provider-omega/chat-mini"},
                {"purpose": "embed_chunk", "modelName": "provider-omega/alpha-embedding-small"},
                {"purpose": "query_compile", "modelName": "provider-omega/chat-mini"},
                {"purpose": "query_answer", "modelName": "provider-omega/chat-vision"},
                {"purpose": "agent", "modelName": "provider-omega/chat-vision"}
            ]
        }),
        "provider-zeta" => serde_json::json!({
            "bootstrapPresets": [
                {
                    "purpose": "extract_graph",
                    "modelName": "zeta-chat",
                    "systemPrompt": "Extract only supported facts.",
                    "extraParametersJson": {"sample": {"enabled": true}}
                }
            ]
        }),
        _ => serde_json::json!({}),
    }
}

#[test]
fn parses_allowed_binding_purposes_from_default_roles() {
    let metadata = serde_json::json!({
        "defaultRoles": ["extract_graph", "query_answer"]
    });
    let purposes = parse_allowed_binding_purposes(&metadata, "model sample defaultRoles")
        .expect("defaultRoles should parse");
    assert_eq!(purposes, vec![AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer]);
}

#[test]
fn removed_binding_purposes_are_rejected_with_role_context() {
    let metadata = serde_json::json!({
        "defaultRoles": ["rerank"]
    });

    let error = parse_allowed_binding_purposes(&metadata, "model sample defaultRoles")
        .expect_err("unknown roles must fail closed");

    assert!(matches!(error, ApiError::BadRequest(_)));
    let detail = format!("{error:?}");
    assert!(detail.contains("model sample defaultRoles[0]"));
    assert!(detail.contains("rerank"));
}

#[test]
fn non_string_binding_purposes_are_rejected_with_role_context() {
    let metadata = serde_json::json!({
        "defaultRoles": [42]
    });
    let error = parse_allowed_binding_purposes(&metadata, "model sample defaultRoles")
        .expect_err("non-string roles must fail closed");

    assert!(matches!(error, ApiError::BadRequest(_)));
    assert!(format!("{error:?}").contains("model sample defaultRoles[0]"));
}

#[test]
fn empty_binding_purposes_are_allowed_only_for_disabled_catalog_rows() {
    let disabled = sample_model_row("disabled", serde_json::json!({ "defaultRoles": [] }));
    let mapped = map_model_row(disabled).expect("disabled historical rows may remain unmapped");
    assert!(mapped.allowed_binding_purposes.is_empty());

    let active = sample_model_row("active", serde_json::json!({ "defaultRoles": [] }));
    let error = map_model_row(active).expect_err("active models require a canonical purpose");
    assert!(matches!(error, ApiError::BadRequest(_)));
    assert!(format!("{error:?}").contains("sample-model"));
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
fn document_understanding_requires_multimodal_model_and_provider() {
    let text_model = sample_model(vec![AiBindingPurpose::ExtractText]);
    let text_error = validate_model_binding_purpose(AiBindingPurpose::ExtractText, &text_model)
        .expect_err("document understanding must reject text-only models");
    assert!(format!("{text_error:?}").contains("multimodal"));

    let mut multimodal_model = text_model;
    multimodal_model.modality_kind = "multimodal".to_string();
    validate_model_binding_purpose(AiBindingPurpose::ExtractText, &multimodal_model)
        .expect("document understanding accepts a typed multimodal model");

    let mut provider = sample_provider("provider-alpha");
    provider.capabilities.vision = ProviderCapabilityState::Unsupported;
    validate_provider_capability_for_binding(&provider, AiBindingPurpose::ExtractText)
        .expect_err("document understanding must reject providers without visual capability");
    provider.capabilities.vision = ProviderCapabilityState::Supported;
    validate_provider_capability_for_binding(&provider, AiBindingPurpose::ExtractText)
        .expect("document understanding accepts a visual-capable provider");
    provider.capabilities.chat = ProviderCapabilityState::Unknown;
    validate_provider_capability_for_binding(&provider, AiBindingPurpose::ExtractText)
        .expect_err("document understanding must also require a declared chat capability");
}

#[test]
fn agent_requires_explicit_chat_and_tool_provider_capabilities() {
    let mut provider = sample_provider("provider-beta");

    validate_provider_capability_for_binding(&provider, AiBindingPurpose::QueryAnswer)
        .expect("ordinary answer generation only requires the declared chat capability");
    validate_provider_capability_for_binding(&provider, AiBindingPurpose::Agent)
        .expect_err("an unknown tools capability must fail closed for Agent");

    provider.capabilities.tools = ProviderCapabilityState::Unsupported;
    validate_provider_capability_for_binding(&provider, AiBindingPurpose::Agent)
        .expect_err("an unsupported tools capability must reject Agent");

    provider.capabilities.tools = ProviderCapabilityState::Supported;
    validate_provider_capability_for_binding(&provider, AiBindingPurpose::Agent)
        .expect("Agent accepts a provider that explicitly supports chat and tools");

    provider.capabilities.chat = ProviderCapabilityState::Unknown;
    validate_provider_capability_for_binding(&provider, AiBindingPurpose::Agent)
        .expect_err("Agent must fail closed when chat support is unknown");
}

#[test]
fn generative_binding_purposes_require_chat_model_capability() {
    let mut model = sample_model(vec![AiBindingPurpose::Agent]);
    model.capability_kind = "embedding".to_string();

    let error = validate_model_binding_purpose(AiBindingPurpose::Agent, &model)
        .expect_err("an embedding model must never be accepted as an Agent model");

    assert!(format!("{error:?}").contains("chat"));
}

#[test]
fn query_understanding_accepts_query_compile_only_models() {
    let model = sample_model(vec![AiBindingPurpose::QueryCompile]);
    validate_model_binding_purpose(AiBindingPurpose::QueryCompile, &model)
        .expect("query compile and rerank share one typed model role");
}

#[test]
fn model_discovery_chat_path_creates_text_model_roles() {
    let provider = sample_provider("provider-beta");
    let signature = signature_for_capability(&provider, "chat");
    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "text");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
        ]
    );
}

#[test]
fn model_discovery_vision_path_creates_multimodal_model_roles() {
    let provider = sample_provider("provider-beta");
    let signature = signature_for_capability(&provider, "vision");
    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "multimodal");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[
            AiBindingPurpose::ExtractText,
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
        ]
    );
}

#[test]
fn model_discovery_embedding_path_creates_embedding_model_roles() {
    let provider = sample_provider("provider-beta");
    let signature = signature_for_capability(&provider, "embedding");
    assert_eq!(signature.capability_kind, "embedding");
    assert_eq!(signature.modality_kind, "text");
    assert_eq!(signature.allowed_binding_purposes, &[AiBindingPurpose::EmbedChunk]);
}

#[test]
fn bootstrap_binding_inputs_accept_five_required_purposes() {
    let inputs = vec![
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::ExtractGraph,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::EmbedChunk,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::QueryCompile,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::QueryAnswer,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::Agent,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
    ];

    assert!(bootstrap_binding_inputs_cover_required_purposes(&inputs));
    validate_bootstrap_binding_inputs_cover_required_purposes(&inputs)
        .expect("document understanding is optional for text-only bootstrap");
}

#[test]
fn bootstrap_binding_inputs_reject_missing_explicit_agent_purpose() {
    let inputs = vec![
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::ExtractGraph,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::EmbedChunk,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::QueryCompile,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiBindingInput {
            binding_purpose: AiBindingPurpose::QueryAnswer,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
    ];

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
fn bootstrap_agent_preset_fails_closed_when_provider_tools_are_unknown() {
    let provider = sample_provider("provider-beta");
    let model = ModelCatalogEntry {
        id: Uuid::now_v7(),
        provider_catalog_id: provider.id,
        model_name: "beta-chat-small".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "text".to_string(),
        lifecycle_state: "active".to_string(),
        metadata_json: serde_json::json!({}),
        allowed_binding_purposes: vec![AiBindingPurpose::Agent],
        context_window: None,
        max_output_tokens: None,
    };

    let descriptors = resolve_bootstrap_provider_binding_descriptors(
        &provider,
        std::slice::from_ref(&provider),
        &[model],
    )
    .expect("typed bootstrap profile should still parse");

    assert!(descriptors.is_empty(), "an Agent preset needs explicit tools support");
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
    let model = ModelCatalogEntry {
        id: Uuid::now_v7(),
        provider_catalog_id: provider.id,
        model_name: "agent-capable-model".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "text".to_string(),
        lifecycle_state: "active".to_string(),
        metadata_json: serde_json::json!({}),
        allowed_binding_purposes: vec![AiBindingPurpose::Agent, AiBindingPurpose::QueryAnswer],
        context_window: None,
        max_output_tokens: None,
    };
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

#[test]
fn bootstrap_bundle_uses_expected_provider_alpha_models() {
    let provider = sample_provider("provider-alpha");
    let extract_graph_model_id = Uuid::now_v7();
    let query_answer_model_id = Uuid::now_v7();
    let embed_model_id = Uuid::now_v7();
    let models = vec![
        ModelCatalogEntry {
            id: extract_graph_model_id,
            provider_catalog_id: provider.id,
            model_name: "alpha-chat-mini".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: query_answer_model_id,
            provider_catalog_id: provider.id,
            model_name: "alpha-chat-plus".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: embed_model_id,
            provider_catalog_id: provider.id,
            model_name: "alpha-embedding-large".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
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
        Some(0.3)
    );
}

#[test]
fn bootstrap_binding_descriptors_preserve_typed_provider_settings() {
    let provider = sample_provider("provider-zeta");
    let model = ModelCatalogEntry {
        id: Uuid::now_v7(),
        provider_catalog_id: provider.id,
        model_name: "zeta-chat".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "text".to_string(),
        lifecycle_state: "active".to_string(),
        metadata_json: serde_json::json!({}),
        allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
        context_window: None,
        max_output_tokens: None,
    };

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
        ModelCatalogEntry {
            id: graph_model_id,
            provider_catalog_id: provider.id,
            model_name: "provider-gamma-chat-flash".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: runtime_model_id,
            provider_catalog_id: provider.id,
            model_name: "gamma-chat-max".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: embed_model_id,
            provider_catalog_id: provider.id,
            model_name: "gamma-embedding-large".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: vision_model_id,
            provider_catalog_id: provider.id,
            model_name: "provider-gamma-vl-max".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
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
    let models = vec![ModelCatalogEntry {
        id: Uuid::now_v7(),
        provider_catalog_id: provider.id,
        model_name: "provider-delta-chat".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "text".to_string(),
        lifecycle_state: "active".to_string(),
        metadata_json: serde_json::json!({}),
        allowed_binding_purposes: vec![
            AiBindingPurpose::ExtractText,
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
            AiBindingPurpose::Agent,
        ],
        context_window: None,
        max_output_tokens: None,
    }];

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
fn model_discovery_chat_signature_is_text_query_capable() {
    let provider = sample_provider("provider-alpha");
    let signature = signature_for_capability(&provider, "chat");

    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "text");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
            AiBindingPurpose::Agent,
        ]
    );
}

#[test]
fn model_discovery_vision_signature_is_multimodal_and_query_capable() {
    let provider = sample_provider("provider-gamma");
    let signature = signature_for_capability(&provider, "vision");

    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "multimodal");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[
            AiBindingPurpose::ExtractText,
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
            AiBindingPurpose::Agent,
        ]
    );
}

#[test]
fn model_discovery_embedding_and_vision_capabilities_classify_correctly() {
    let provider = sample_provider("provider-beta");
    let embedding = signature_for_capability(&provider, "embedding");
    assert_eq!(embedding.capability_kind, "embedding");
    assert_eq!(embedding.modality_kind, "text");
    assert_eq!(embedding.allowed_binding_purposes, &[AiBindingPurpose::EmbedChunk]);

    let vision = signature_for_capability(&provider, "vision");
    assert_eq!(vision.capability_kind, "chat");
    assert_eq!(vision.modality_kind, "multimodal");
    assert_eq!(
        vision.allowed_binding_purposes,
        &[
            AiBindingPurpose::ExtractText,
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
        ]
    );
}

#[test]
fn model_discovery_chat_capability_remains_text_only_without_vision_path() {
    let provider = sample_provider("provider-delta");
    let signature = signature_for_capability(&provider, "chat");

    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "text");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
            AiBindingPurpose::Agent,
        ]
    );
}

#[test]
fn model_discovery_rejects_unknown_capability_kind() {
    let provider = sample_provider("provider-alpha");
    let result = discovered_provider_model_signature_for_capability(&provider, "audio");

    assert!(matches!(result, Err(ApiError::BadRequest(_))));
}

#[test]
fn discovered_router_models_use_path_capabilities_not_model_name() {
    let provider = sample_provider("synthetic-router");
    let chat = signature_for_capability(&provider, "chat");
    assert_eq!(chat.capability_kind, "chat");
    assert_eq!(chat.modality_kind, "text");

    let prefixed_chat = signature_for_capability(&provider, "vision");
    assert_eq!(prefixed_chat.capability_kind, "chat");
    assert_eq!(prefixed_chat.modality_kind, "multimodal");

    let embedding = signature_for_capability(&provider, "embedding");
    assert_eq!(embedding.capability_kind, "embedding");
    assert_eq!(embedding.allowed_binding_purposes, &[AiBindingPurpose::EmbedChunk]);
}

#[test]
fn discovered_router_paths_respect_unsupported_capabilities() {
    let mut provider = sample_provider("synthetic-router");
    provider.capabilities.embeddings = ProviderCapabilityState::Unsupported;
    provider.capabilities.vision = ProviderCapabilityState::Unsupported;

    assert!(
        discovered_provider_model_signature_for_capability(&provider, "embedding")
            .expect("embedding capability kind is known")
            .is_none(),
        "embedding paths must not become binding models when embeddings are unsupported"
    );

    let chat = signature_for_capability(&provider, "chat");
    assert_eq!(chat.capability_kind, "chat");
    assert_eq!(chat.modality_kind, "text");
    assert_eq!(
        chat.allowed_binding_purposes,
        &[
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
            AiBindingPurpose::Agent,
        ]
    );
}

#[test]
fn discovered_router_opaque_model_ids_are_kept_by_declared_path_capability() {
    let provider = sample_provider("synthetic-router");
    let signature = signature_for_capability(&provider, "chat");

    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "text");
}

#[test]
fn bootstrap_bundle_uses_expected_provider_beta_models() {
    let mut provider = sample_provider("provider-beta");
    provider.capabilities.tools = ProviderCapabilityState::Supported;
    let graph_model_id = Uuid::now_v7();
    let embed_model_id = Uuid::now_v7();
    let vision_model_id = Uuid::now_v7();
    let models = vec![
        ModelCatalogEntry {
            id: graph_model_id,
            provider_catalog_id: provider.id,
            model_name: "beta-chat-small".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: embed_model_id,
            provider_catalog_id: provider.id,
            model_name: "beta-embedding-small".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: vision_model_id,
            provider_catalog_id: provider.id,
            model_name: "beta-chat-vision".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
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
        ModelCatalogEntry {
            id: graph_model_id,
            provider_catalog_id: provider.id,
            model_name: "provider-omega/chat-mini".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: answer_model_id,
            provider_catalog_id: provider.id,
            model_name: "provider-omega/chat-vision".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: embed_model_id,
            provider_catalog_id: provider.id,
            model_name: "provider-omega/alpha-embedding-small".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
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
        ModelCatalogEntry {
            id: graph_model_id,
            provider_catalog_id: provider.id,
            model_name: "beta-chat-small".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: embed_model_id,
            provider_catalog_id: provider.id,
            model_name: "beta-embedding-small".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
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
fn preserves_provider_alpha_compatible_base_url_path() {
    let provider = sample_provider("provider-beta");

    assert_eq!(
        canonicalize_provider_base_url(&provider, "http://localhost:11434/v1")
            .expect("provider-beta Provider Alpha-compatible base path should normalize"),
        "http://localhost:11434/v1"
    );
    assert_eq!(
        canonicalize_provider_base_url(&provider, "http://localhost:11434/api")
            .expect("non-canonical provider-beta paths should not be rewritten"),
        "http://localhost:11434/api"
    );
}

#[test]
fn provider_base_url_policy_blocks_hosted_overrides_and_allows_local_overrides() {
    let provider = sample_provider("provider-alpha");
    let error = normalize_provider_base_url_input(&provider, Some("https://override.example/v1"))
        .expect_err("hosted providers must reject forbidden baseUrl overrides");
    assert!(matches!(error, ApiError::BadRequest(_)));

    let local_provider = sample_provider("provider-beta");
    let local_normalized =
        normalize_provider_base_url_input(&local_provider, Some("http://localhost:11434/v1"))
            .expect("local providers should still accept explicit baseUrl overrides");
    assert_eq!(local_normalized.as_deref(), Some("http://localhost:11434/v1"));

    let mut private_provider = provider.clone();
    private_provider.base_url_policy.allow_override = true;
    let private_error = canonicalize_provider_base_url(&private_provider, "https://127.0.0.1/v1")
        .expect_err("hosted providers should reject private network base URLs");
    assert!(matches!(private_error, ApiError::BadRequest(_)));

    let userinfo_error =
        canonicalize_provider_base_url(&private_provider, "https://userinfo@example.com/v1")
            .expect_err("provider base URLs must not carry userinfo");
    assert!(matches!(userinfo_error, ApiError::BadRequest(_)));

    let query_error =
        canonicalize_provider_base_url(&private_provider, "https://example.com/v1?marker=opaque")
            .expect_err("provider base URLs must not carry query strings");
    assert!(matches!(query_error, ApiError::BadRequest(_)));
}

#[test]
fn openai_provider_accepts_base_url_override_for_compatible_gateways() {
    let mut provider = sample_provider("provider-alpha");
    provider.provider_kind = "openai".to_string();
    provider.base_url_policy.allow_override = true;

    let normalized =
        normalize_provider_base_url_input(&provider, Some("https://openai.bothub.ru/v1"))
            .expect("openai provider should accept explicit compatible baseUrl override");
    assert_eq!(normalized.as_deref(), Some("https://openai.bothub.ru/v1"));
}

#[test]
fn hosted_base_url_update_clears_legacy_override_and_runtime_rejects_it() {
    let provider = sample_provider("provider-epsilon");

    let created = provider_credential_base_url_for_create(&provider, None)
        .expect("fixed hosted providers should not store provider default on create");
    assert_eq!(created, None);

    let stored = provider_credential_base_url_for_update(
        &provider,
        Some("https://stale-host.example/v1"),
        None,
    )
    .expect("fixed hosted providers should clear stored credential baseUrl during update");
    assert_eq!(stored, None);

    let runtime_error = runtime_provider_base_url(&provider, Some("https://stale-host.example/v1"))
        .expect_err("runtime must fail closed on a forbidden persisted override");
    assert!(matches!(runtime_error, ApiError::BadRequest(_)));
}

#[test]
fn allowed_override_update_preserves_omitted_url_but_not_hidden_key_across_origins() {
    let provider = sample_provider("provider-beta");
    let existing = "http://localhost:11434/v1";

    let preserved = provider_credential_base_url_for_update(&provider, Some(existing), None)
        .expect("omitting an allowed override should preserve it");
    assert_eq!(preserved.as_deref(), Some(existing));

    validate_provider_base_url_key_reuse(
        &provider,
        Some(existing),
        Some("http://localhost:11434/api"),
        true,
        false,
    )
    .expect("the same canonical authority may reuse the existing key");

    let authority_error = validate_provider_base_url_key_reuse(
        &provider,
        Some(existing),
        Some("https://gateway.example/v1"),
        true,
        false,
    )
    .expect_err("a hidden key must never move to a different authority");
    assert!(matches!(authority_error, ApiError::BadRequest(_)));

    validate_provider_base_url_key_reuse(
        &provider,
        Some(existing),
        Some("https://gateway.example/v1"),
        true,
        true,
    )
    .expect("an explicitly supplied replacement key may be validated on a new authority");
}

#[test]
fn detects_loopback_base_urls() {
    assert!(is_loopback_base_url("http://localhost:11434/v1"));
    assert!(is_loopback_base_url("http://127.0.0.1:11434/v1"));
    assert!(!is_loopback_base_url("http://host.docker.internal:11434/v1"));
}

#[test]
fn configured_bootstrap_bindings_inherit_provider_bundle_tuning_when_models_match() {
    let provider = sample_provider("provider-alpha");
    let model = ModelCatalogEntry {
        id: Uuid::now_v7(),
        provider_catalog_id: provider.id,
        model_name: "alpha-chat-mini".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "multimodal".to_string(),
        lifecycle_state: "active".to_string(),
        metadata_json: serde_json::json!({}),
        allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
        context_window: None,
        max_output_tokens: None,
    };
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
    assert_eq!(binding_inputs[0].temperature, Some(0.3));
    assert_eq!(binding_inputs[0].top_p, Some(0.9));
}

#[test]
fn bootstrap_bundle_omits_incomplete_provider_profiles() {
    let provider_delta = sample_provider("provider-delta");
    let models = vec![ModelCatalogEntry {
        id: Uuid::now_v7(),
        provider_catalog_id: provider_delta.id,
        model_name: "provider-delta-chat".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "text".to_string(),
        lifecycle_state: "active".to_string(),
        metadata_json: serde_json::json!({}),
        allowed_binding_purposes: vec![
            AiBindingPurpose::ExtractText,
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::QueryAnswer,
        ],
        context_window: None,
        max_output_tokens: None,
    }];

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
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: provider_delta.id,
            model_name: "provider-delta-chat".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: provider_alpha_provider.id,
            model_name: "alpha-embedding-large".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: provider_alpha_provider.id,
            model_name: "alpha-chat-plus".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
            ],
            context_window: None,
            max_output_tokens: None,
        },
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
fn required_bootstrap_bundle_is_self_contained_without_document_understanding() {
    let provider = sample_provider("provider-alpha");
    let models = vec![
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: provider.id,
            model_name: "alpha-chat-mini".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: provider.id,
            model_name: "alpha-embedding-large".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: provider.id,
            model_name: "alpha-chat-plus".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ],
            context_window: None,
            max_output_tokens: None,
        },
    ];
    let bundle = resolve_bootstrap_provider_binding_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("bundle should resolve")
    .expect("bundle should be available");

    assert_eq!(bundle.bindings.len(), 5);
    assert!(bootstrap_bundle_is_self_contained(&bundle));
    assert_eq!(bundle.ui_hints, serde_json::json!({"accent": "neutral"}));
}

#[test]
fn vector_dimension_cache_tracks_embed_chunk_purpose() {
    assert!(super::binding_affects_vector_index_dimension(AiBindingPurpose::EmbedChunk));
    assert!(!super::binding_affects_vector_index_dimension(AiBindingPurpose::ExtractGraph));
    assert!(!super::binding_affects_vector_index_dimension(AiBindingPurpose::QueryCompile));
}

#[test]
fn env_provider_credential_bootstrap_skips_typed_provider_validation_failures() {
    for failure_kind in [
        ProviderCredentialValidationFailureKind::Rejected,
        ProviderCredentialValidationFailureKind::Transport,
        ProviderCredentialValidationFailureKind::LoopbackUnreachable,
    ] {
        let error = provider_credential_validation_error(failure_kind, "opaque detail");
        assert!(is_provider_credential_validation_error(&error), "{failure_kind:?}");
    }
}

#[test]
fn env_provider_credential_bootstrap_does_not_classify_error_prose() {
    let prose_only = ApiError::BadRequest(
        "provider credential validation failed for Provider Alpha at /models".to_string(),
    );

    assert!(!is_provider_credential_validation_error(&prose_only));
    assert!(!is_provider_credential_validation_error(&ApiError::Internal));
    assert!(!is_provider_credential_validation_error(&ApiError::Conflict(
        "duplicate credential label".to_string()
    )));
    assert!(!is_provider_credential_validation_error(&ApiError::NotFound(
        "provider_catalog missing".to_string()
    )));
}

#[test]
fn chat_round_trip_rejects_only_typed_authentication_statuses() {
    for status in [reqwest::StatusCode::UNAUTHORIZED, reqwest::StatusCode::FORBIDDEN] {
        let error = anyhow::Error::new(crate::integrations::retry::ProviderCallError::http_status(
            "provider-alpha",
            status,
            &reqwest::header::HeaderMap::new(),
            "opaque",
        ));

        assert_eq!(
            chat_round_trip_validation_failure_kind(&error),
            ChatRoundTripValidationFailureKind::CredentialRejected,
        );
    }
}

#[test]
fn chat_round_trip_maps_typed_unavailability_without_rejecting_credentials() {
    for status in [
        reqwest::StatusCode::REQUEST_TIMEOUT,
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        reqwest::StatusCode::BAD_GATEWAY,
    ] {
        let error = anyhow::Error::new(crate::integrations::retry::ProviderCallError::http_status(
            "provider-alpha",
            status,
            &reqwest::header::HeaderMap::new(),
            "opaque",
        ));

        assert_eq!(
            chat_round_trip_validation_failure_kind(&error),
            ChatRoundTripValidationFailureKind::ProviderUnavailable,
        );
    }

    let source = reqwest::Client::new()
        .get("://")
        .build()
        .expect_err("synthetic malformed URL must create a typed reqwest error");
    let error = anyhow::Error::new(crate::integrations::retry::ProviderCallError::transport(
        "opaque", source,
    ));
    assert_eq!(
        chat_round_trip_validation_failure_kind(&error),
        ChatRoundTripValidationFailureKind::ProviderUnavailable,
    );
}

#[test]
fn chat_round_trip_does_not_infer_credentials_from_misleading_prose() {
    let protocol_error =
        anyhow::Error::new(crate::integrations::retry::ProviderCallError::protocol(
            "status=401 credential rejected transport timeout",
        ));
    let untyped_error = anyhow::anyhow!("status=403 invalid api key");
    let non_authentication_status =
        anyhow::Error::new(crate::integrations::retry::ProviderCallError::http_status(
            "provider-alpha",
            reqwest::StatusCode::BAD_REQUEST,
            &reqwest::header::HeaderMap::new(),
            "credential rejected",
        ));

    assert_eq!(
        chat_round_trip_validation_failure_kind(&protocol_error),
        ChatRoundTripValidationFailureKind::Internal,
    );
    assert_eq!(
        chat_round_trip_validation_failure_kind(&untyped_error),
        ChatRoundTripValidationFailureKind::Internal,
    );
    assert_eq!(
        chat_round_trip_validation_failure_kind(&non_authentication_status),
        ChatRoundTripValidationFailureKind::Internal,
    );
}

#[test]
fn env_provider_credential_bootstrap_compares_decrypted_values_not_randomized_envelopes() {
    let encoded_key = STANDARD.encode([61_u8; 32]);
    let cipher = CredentialCipher::from_optional_base64(Some(&encoded_key)).expect("valid key");
    let account_id = Uuid::now_v7();
    let encrypted = cipher
        .encrypt(SecretPurpose::AiAccountApiKey, account_id, "current-key")
        .expect("encrypt configured key");

    assert!(
        !bootstrap_env_credential_needs_sync(
            &cipher,
            account_id,
            Some(encrypted.as_str()),
            "current-key",
        )
        .expect("matching encrypted credential should compare")
    );
    assert!(
        bootstrap_env_credential_needs_sync(
            &cipher,
            account_id,
            Some(encrypted.as_str()),
            "rotated-key",
        )
        .expect("rotated encrypted credential should compare")
    );
    assert!(
        !bootstrap_env_credential_needs_sync(
            &cipher,
            account_id,
            Some("current-key"),
            "current-key",
        )
        .expect("legacy plaintext remains comparable")
    );
    assert!(
        bootstrap_env_credential_needs_sync(&cipher, account_id, None, "current-key")
            .expect("missing credential requires sync")
    );
    assert!(
        bootstrap_env_credential_needs_sync(
            &cipher,
            Uuid::now_v7(),
            Some(encrypted.as_str()),
            "current-key",
        )
        .is_err(),
        "copying a v2 envelope to another account must fail closed"
    );
}

#[test]
fn runtime_binding_batch_deduplicates_purposes_in_first_seen_order() {
    let purposes = deduplicate_binding_purposes(&[
        AiBindingPurpose::QueryAnswer,
        AiBindingPurpose::EmbedChunk,
        AiBindingPurpose::QueryAnswer,
        AiBindingPurpose::ExtractText,
        AiBindingPurpose::EmbedChunk,
    ]);

    assert_eq!(
        purposes,
        vec![
            AiBindingPurpose::QueryAnswer,
            AiBindingPurpose::EmbedChunk,
            AiBindingPurpose::ExtractText,
        ]
    );
    assert!(deduplicate_binding_purposes(&[]).is_empty());
}

#[test]
fn binding_embedding_dimensions_take_precedence_over_catalog_metadata() {
    let dimensions = resolve_effective_embedding_dimensions(
        AiBindingPurpose::EmbedChunk,
        &serde_json::json!({"dimensions": 384}),
        &serde_json::json!({"dimensions": 768}),
    )
    .expect("valid embedding dimensions")
    .expect("configured embedding dimensions");

    assert_eq!(dimensions.get(), 384);
}

#[test]
fn catalog_embedding_dimensions_are_used_without_mutating_request_parameters() {
    let binding_parameters = serde_json::json!({"encoding_format": "float"});
    let dimensions = resolve_effective_embedding_dimensions(
        AiBindingPurpose::EmbedChunk,
        &binding_parameters,
        &serde_json::json!({"dimensions": 768}),
    )
    .expect("valid catalog embedding dimensions")
    .expect("catalog embedding dimensions");

    assert_eq!(dimensions.get(), 768);
    assert!(binding_parameters.get("dimensions").is_none());
}

#[test]
fn invalid_explicit_embedding_dimensions_fail_closed_instead_of_falling_back() {
    for invalid_dimensions in [
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!("384"),
        serde_json::json!(4_001),
    ] {
        let result = resolve_effective_embedding_dimensions(
            AiBindingPurpose::EmbedChunk,
            &serde_json::json!({"dimensions": invalid_dimensions}),
            &serde_json::json!({"dimensions": 768}),
        );

        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }
}

#[test]
fn invalid_catalog_embedding_dimensions_fail_closed() {
    for invalid_dimensions in [
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!("384"),
        serde_json::json!(4_001),
    ] {
        let result = resolve_effective_embedding_dimensions(
            AiBindingPurpose::EmbedChunk,
            &serde_json::json!({}),
            &serde_json::json!({"dimensions": invalid_dimensions}),
        );

        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }
}
