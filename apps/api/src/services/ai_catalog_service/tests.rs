use super::bootstrap::{
    align_query_retrieve_with_embed_chunk, missing_bootstrap_model_list_models,
};
use super::provider_validation::normalize_provider_base_url_input;
use super::{
    BootstrapAiBindingInput, BootstrapAiCredentialSource,
    bootstrap_binding_inputs_cover_required_purposes, bootstrap_bundle_is_self_contained,
    canonicalize_provider_base_url, discovered_provider_model_signature_for_capability,
    is_loopback_base_url, is_provider_credential_validation_error, merge_provider_runtime_profile,
    parse_allowed_binding_purposes, provider_credential_base_url_for_create,
    provider_credential_base_url_for_update, resolve_bootstrap_provider_binding_bundle,
    resolve_bootstrap_provider_binding_descriptors, resolve_configured_bootstrap_binding_inputs,
    runtime_provider_base_url, validate_bootstrap_binding_inputs_cover_required_purposes,
    validate_model_binding_purpose,
};
use crate::app::config::UiBootstrapAiBindingDefault;
use crate::domains::ai::{AiBindingPurpose, ModelCatalogEntry, ProviderCatalogEntry};
use crate::domains::provider_profiles::{
    OPENAI_COMPATIBLE_RUNTIME_KIND, ProviderAuthScheme, ProviderBaseUrlMode, ProviderBaseUrlPolicy,
    ProviderCapabilities, ProviderCapabilityState, ProviderCredentialPolicy,
    ProviderCredentialValidationMode, ProviderModelDiscovery, ProviderModelDiscoveryMode,
    ProviderModelDiscoveryPath, ProviderProfile, ProviderRuntimeProfile,
    ProviderStructuredOutputMode, ProviderTokenLimitParameter,
};
use crate::interfaces::http::router_support::ApiError;
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
fn runtime_profile_merge_overwrites_stale_preset_metadata() {
    let provider = sample_provider("synthetic-router");
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
}

fn bootstrap_capability_flags(provider_kind: &str) -> serde_json::Value {
    match provider_kind {
        "provider-alpha" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_text", "modelName": "alpha-chat-mini"},
                {"purpose": "extract_graph", "modelName": "alpha-chat-mini"},
                {"purpose": "embed_chunk", "modelName": "alpha-embedding-large"},
                {"purpose": "query_compile", "modelName": "alpha-chat-plus"},
                {"purpose": "query_retrieve", "modelName": "alpha-embedding-large"},
                {"purpose": "query_answer", "modelName": "alpha-chat-plus"},
                {"purpose": "vision", "modelName": "alpha-chat-plus"}
            ],
            "uiHints": {"accent": "neutral"}
        }),
        "provider-beta" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_text", "modelName": "beta-chat-small"},
                {"purpose": "extract_graph", "modelName": "beta-chat-small"},
                {"purpose": "embed_chunk", "modelName": "beta-embedding-small"},
                {"purpose": "query_compile", "modelName": "beta-chat-small"},
                {"purpose": "query_retrieve", "modelName": "beta-embedding-small"},
                {"purpose": "query_answer", "modelName": "beta-chat-small"},
                {"purpose": "vision", "modelName": "beta-chat-vision"}
            ]
        }),
        "provider-gamma" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_text", "modelName": "provider-gamma-chat-flash"},
                {"purpose": "extract_graph", "modelName": "provider-gamma-chat-flash"},
                {"purpose": "embed_chunk", "modelName": "gamma-embedding-large"},
                {"purpose": "query_compile", "modelName": "gamma-chat-max"},
                {"purpose": "query_retrieve", "modelName": "gamma-embedding-large"},
                {"purpose": "query_answer", "modelName": "gamma-chat-max"},
                {"purpose": "vision", "modelName": "provider-gamma-vl-max"}
            ]
        }),
        "provider-delta" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_text", "modelName": "provider-delta-chat"},
                {"purpose": "extract_graph", "modelName": "provider-delta-chat"},
                {"purpose": "query_compile", "modelName": "provider-delta-chat"},
                {"purpose": "query_answer", "modelName": "provider-delta-chat"}
            ]
        }),
        "provider-epsilon" => serde_json::json!({
            "bootstrapPresets": [
                {"purpose": "extract_text", "modelName": "provider-omega/chat-mini"},
                {"purpose": "extract_graph", "modelName": "provider-omega/chat-mini"},
                {"purpose": "embed_chunk", "modelName": "provider-omega/alpha-embedding-small"},
                {"purpose": "query_compile", "modelName": "provider-omega/chat-mini"},
                {"purpose": "query_retrieve", "modelName": "provider-omega/alpha-embedding-small"},
                {"purpose": "query_answer", "modelName": "provider-omega/chat-vision"},
                {"purpose": "vision", "modelName": "provider-omega/chat-vision"}
            ]
        }),
        "provider-zeta" => serde_json::json!({
            "bootstrapPresets": [
                {
                    "purpose": "extract_graph",
                    "modelName": "zeta-chat",
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
    let purposes = parse_allowed_binding_purposes(&metadata).expect("defaultRoles should parse");
    assert_eq!(purposes, vec![AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer]);
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
fn model_discovery_chat_path_creates_text_model_roles() {
    let provider = sample_provider("provider-beta");
    let signature = signature_for_capability(&provider, "chat");
    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "text");
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
            AiBindingPurpose::Vision,
            AiBindingPurpose::Agent,
        ]
    );
}

#[test]
fn model_discovery_embedding_path_creates_embedding_model_roles() {
    let provider = sample_provider("provider-beta");
    let signature = signature_for_capability(&provider, "embedding");
    assert_eq!(signature.capability_kind, "embedding");
    assert_eq!(signature.modality_kind, "text");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[AiBindingPurpose::EmbedChunk, AiBindingPurpose::QueryRetrieve]
    );
}

#[test]
fn bootstrap_binding_inputs_accept_required_purposes_without_vision() {
    let embedding_model_id = Uuid::now_v7();
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
            model_catalog_id: embedding_model_id,
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
            binding_purpose: AiBindingPurpose::QueryRetrieve,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: embedding_model_id,
            system_prompt: None,
            temperature: None,
            top_p: None,
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
        .expect("vision is optional for text-only bootstrap");
}

#[test]
fn bootstrap_binding_inputs_reject_mismatched_embedding_models() {
    let mut inputs = vec![
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
            binding_purpose: AiBindingPurpose::QueryRetrieve,
            provider_kind: "provider-alpha".to_string(),
            model_catalog_id: Uuid::now_v7(),
            system_prompt: None,
            temperature: None,
            top_p: None,
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

    assert!(matches!(
        validate_bootstrap_binding_inputs_cover_required_purposes(&inputs),
        Err(ApiError::BadRequest(_))
    ));
    let embed_model_id = inputs
        .iter()
        .find(|input| input.binding_purpose == AiBindingPurpose::EmbedChunk)
        .unwrap()
        .model_catalog_id;
    inputs
        .iter_mut()
        .find(|input| input.binding_purpose == AiBindingPurpose::QueryRetrieve)
        .unwrap()
        .model_catalog_id = embed_model_id;

    validate_bootstrap_binding_inputs_cover_required_purposes(&inputs).unwrap();
}

#[test]
fn bootstrap_binding_inputs_reject_missing_required_purpose() {
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
                AiBindingPurpose::Vision,
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
                AiBindingPurpose::Vision,
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
            allowed_binding_purposes: vec![
                AiBindingPurpose::EmbedChunk,
                AiBindingPurpose::QueryRetrieve,
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
    .expect("provider-alpha bundle should resolve")
    .expect("provider-alpha bundle should be available");

    assert_eq!(bundle.provider_kind, "provider-alpha");
    assert_eq!(bundle.bindings.len(), 8);
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractText)
            .map(|binding| binding.model_name.as_str()),
        Some("alpha-chat-mini")
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
            .find(|binding| binding.binding_purpose == AiBindingPurpose::QueryRetrieve)
            .map(|binding| binding.model_name.as_str()),
        Some("alpha-embedding-large")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::QueryRetrieve)
            .and_then(|binding| binding.temperature),
        None
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
fn bootstrap_binding_descriptors_preserve_provider_extra_parameters() {
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
            allowed_binding_purposes: vec![
                AiBindingPurpose::EmbedChunk,
                AiBindingPurpose::QueryRetrieve,
            ],
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
                AiBindingPurpose::Vision,
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
    assert_eq!(bundle.bindings.len(), 8);
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractText)
            .map(|binding| binding.model_name.as_str()),
        Some("provider-gamma-chat-flash")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::QueryCompile)
            .map(|binding| binding.model_name.as_str()),
        Some("gamma-chat-max")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::QueryRetrieve)
            .map(|binding| binding.model_name.as_str()),
        Some("gamma-embedding-large")
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
    assert!(descriptors.iter().any(|binding| {
        binding.binding_purpose == AiBindingPurpose::ExtractText
            && binding.model_name == "provider-delta-chat"
    }));
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
            AiBindingPurpose::ExtractText,
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
            AiBindingPurpose::Vision,
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
    assert_eq!(
        embedding.allowed_binding_purposes,
        &[AiBindingPurpose::EmbedChunk, AiBindingPurpose::QueryRetrieve]
    );

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
            AiBindingPurpose::Vision,
            AiBindingPurpose::Agent,
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
            AiBindingPurpose::ExtractText,
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
    assert_eq!(
        embedding.allowed_binding_purposes,
        &[AiBindingPurpose::EmbedChunk, AiBindingPurpose::QueryRetrieve]
    );
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
            AiBindingPurpose::ExtractText,
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
    let provider = sample_provider("provider-beta");
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
            model_name: "beta-embedding-small".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            lifecycle_state: "active".to_string(),
            metadata_json: serde_json::json!({}),
            allowed_binding_purposes: vec![
                AiBindingPurpose::EmbedChunk,
                AiBindingPurpose::QueryRetrieve,
            ],
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
                AiBindingPurpose::Vision,
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
    assert_eq!(bundle.bindings.len(), 8);
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractText)
            .map(|binding| binding.model_name.as_str()),
        Some("beta-chat-small")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractGraph)
            .map(|binding| binding.model_name.as_str()),
        Some("beta-chat-small")
    );
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::Vision)
            .map(|binding| binding.model_name.as_str()),
        Some("beta-chat-vision")
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
                AiBindingPurpose::Vision,
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
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Vision,
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
            allowed_binding_purposes: vec![
                AiBindingPurpose::EmbedChunk,
                AiBindingPurpose::QueryRetrieve,
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
    .expect("provider-epsilon bundle should resolve")
    .expect("provider-epsilon bundle should be available");

    assert_eq!(bundle.provider_kind, "provider-epsilon");
    assert_eq!(bundle.bindings.len(), 8);
    assert!(bootstrap_bundle_is_self_contained(&bundle));
    assert_eq!(
        bundle
            .bindings
            .iter()
            .find(|binding| binding.binding_purpose == AiBindingPurpose::ExtractText)
            .map(|binding| binding.model_name.as_str()),
        Some("provider-omega/chat-mini")
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
            allowed_binding_purposes: vec![
                AiBindingPurpose::EmbedChunk,
                AiBindingPurpose::QueryRetrieve,
            ],
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
fn hosted_and_local_providers_accept_base_url_overrides() {
    let provider = sample_provider("provider-alpha");
    let normalized =
        normalize_provider_base_url_input(&provider, Some("https://override.example/v1"))
            .expect("hosted providers should accept explicit baseUrl overrides");
    assert_eq!(normalized.as_deref(), Some("https://override.example/v1"));

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

    let normalized =
        normalize_provider_base_url_input(&provider, Some("https://openai.bothub.ru/v1"))
            .expect("openai provider should accept explicit compatible baseUrl override");
    assert_eq!(normalized.as_deref(), Some("https://openai.bothub.ru/v1"));
}

#[test]
fn hosted_base_url_update_clears_empty_override_and_runtime_uses_stored_override() {
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

    let runtime = runtime_provider_base_url(&provider, Some("https://stale-host.example/v1"))
        .expect("runtime should resolve stored credential baseUrl overrides");
    assert_eq!(runtime.as_deref(), Some("https://stale-host.example/v1"));
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
            binding_purpose: "extract_graph".to_string(),
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
            allowed_binding_purposes: vec![
                AiBindingPurpose::EmbedChunk,
                AiBindingPurpose::QueryRetrieve,
            ],
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
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Vision,
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
fn required_bootstrap_bundle_is_self_contained_without_vision() {
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
            allowed_binding_purposes: vec![
                AiBindingPurpose::EmbedChunk,
                AiBindingPurpose::QueryRetrieve,
            ],
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

    assert_eq!(bundle.bindings.len(), 6);
    assert!(bootstrap_bundle_is_self_contained(&bundle));
    assert_eq!(bundle.ui_hints, serde_json::json!({"accent": "neutral"}));
}

#[test]
fn vector_index_counterpart_purpose_maps_embed_and_retrieve_pairs() {
    assert_eq!(
        super::vector_index_counterpart_purpose(AiBindingPurpose::EmbedChunk),
        Some(AiBindingPurpose::QueryRetrieve)
    );
    assert_eq!(
        super::vector_index_counterpart_purpose(AiBindingPurpose::QueryRetrieve),
        Some(AiBindingPurpose::EmbedChunk)
    );
}

#[test]
fn vector_index_counterpart_purpose_is_none_for_non_vector_bindings() {
    assert_eq!(super::vector_index_counterpart_purpose(AiBindingPurpose::ExtractGraph), None);
    assert_eq!(super::vector_index_counterpart_purpose(AiBindingPurpose::QueryCompile), None);
}

#[test]
fn env_provider_credential_bootstrap_skips_only_provider_validation_errors() {
    for message in [
        "provider credential validation failed for Provider Alpha: upstream provider request failed; response details were redacted",
        "provider credential validation failed for Provider Alpha at /models: status=401 Unauthorized",
    ] {
        assert!(
            is_provider_credential_validation_error(&ApiError::BadRequest(message.to_string())),
            "{message}"
        );
    }

    for message in [
        "provider provider-alpha requires a baseUrl",
        "provider provider-alpha does not expose a chat model for credential validation",
        "AI catalog payload violated schema constraints",
    ] {
        assert!(
            !is_provider_credential_validation_error(&ApiError::BadRequest(message.to_string())),
            "{message}"
        );
    }

    assert!(!is_provider_credential_validation_error(&ApiError::Internal));
    assert!(!is_provider_credential_validation_error(&ApiError::Conflict(
        "duplicate credential label".to_string()
    )));
    assert!(!is_provider_credential_validation_error(&ApiError::NotFound(
        "provider_catalog missing".to_string()
    )));
}

fn vector_alignment_input(
    purpose: AiBindingPurpose,
    provider_kind: &str,
    model_catalog_id: Uuid,
) -> BootstrapAiBindingInput {
    BootstrapAiBindingInput {
        binding_purpose: purpose,
        provider_kind: provider_kind.to_string(),
        model_catalog_id,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    }
}

#[test]
fn align_query_retrieve_overwrites_mismatched_fallback_selection() {
    let embed_model_id = Uuid::now_v7();
    let mut selections = vec![
        vector_alignment_input(AiBindingPurpose::EmbedChunk, "provider-alpha", embed_model_id),
        vector_alignment_input(AiBindingPurpose::QueryRetrieve, "provider-beta", Uuid::now_v7()),
    ];
    align_query_retrieve_with_embed_chunk(&mut selections);
    let retrieve = selections
        .iter()
        .find(|input| input.binding_purpose == AiBindingPurpose::QueryRetrieve)
        .expect("query_retrieve selection");
    assert_eq!(retrieve.model_catalog_id, embed_model_id);
    assert_eq!(retrieve.provider_kind, "provider-alpha");
    assert_eq!(selections.len(), 2);
}

#[test]
fn align_query_retrieve_inserts_missing_selection() {
    let embed_model_id = Uuid::now_v7();
    let mut selections = vec![vector_alignment_input(
        AiBindingPurpose::EmbedChunk,
        "provider-alpha",
        embed_model_id,
    )];
    align_query_retrieve_with_embed_chunk(&mut selections);
    let retrieve = selections
        .iter()
        .find(|input| input.binding_purpose == AiBindingPurpose::QueryRetrieve)
        .expect("query_retrieve selection");
    assert_eq!(retrieve.model_catalog_id, embed_model_id);
}

#[test]
fn align_query_retrieve_leaves_selections_without_embed_chunk_untouched() {
    let retrieve_model_id = Uuid::now_v7();
    let mut selections = vec![vector_alignment_input(
        AiBindingPurpose::QueryRetrieve,
        "provider-beta",
        retrieve_model_id,
    )];
    align_query_retrieve_with_embed_chunk(&mut selections);
    assert_eq!(selections.len(), 1);
    assert_eq!(selections[0].model_catalog_id, retrieve_model_id);
}
