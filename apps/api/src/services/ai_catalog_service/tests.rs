use super::{
    BootstrapAiCredentialSource, BootstrapAiPresetInput,
    bootstrap_bundle_has_required_fallback_credentials,
    bootstrap_preset_inputs_cover_canonical_purposes, canonicalize_provider_base_url,
    discovered_provider_model_signature, is_loopback_base_url, parse_allowed_binding_purposes,
    provider_credential_policy, resolve_bootstrap_provider_preset_bundle,
    resolve_configured_bootstrap_preset_inputs, validate_bootstrap_preset_inputs_complete,
    validate_model_binding_purpose,
};
use crate::app::config::{
    UiBootstrapAiBindingDefault, UiBootstrapAiProviderSecret, UiBootstrapAiSetup,
};
use crate::domains::ai::{AiBindingPurpose, ModelCatalogEntry, ProviderCatalogEntry};
use crate::interfaces::http::router_support::ApiError;
use uuid::Uuid;

fn sample_model(allowed_binding_purposes: Vec<AiBindingPurpose>) -> ModelCatalogEntry {
    ModelCatalogEntry {
        id: Uuid::nil(),
        provider_catalog_id: Uuid::nil(),
        model_name: "sample-model".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "text".to_string(),
        allowed_binding_purposes,
        context_window: None,
        max_output_tokens: None,
    }
}

fn sample_provider(provider_kind: &str) -> ProviderCatalogEntry {
    let policy = provider_credential_policy(provider_kind);
    ProviderCatalogEntry {
        id: Uuid::now_v7(),
        provider_kind: provider_kind.to_string(),
        display_name: provider_kind.to_string(),
        api_style: "openai_compatible".to_string(),
        lifecycle_state: "active".to_string(),
        default_base_url: Some("https://example.com/v1".to_string()),
        api_key_required: policy.api_key_required,
        base_url_required: policy.base_url_required,
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
fn discovers_ollama_qwen_text_model_roles() {
    let signature =
        discovered_provider_model_signature("ollama", "qwen3:8b").expect("signature expected");
    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "text");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer]
    );
}

#[test]
fn discovers_ollama_qwen_multimodal_model_roles() {
    let signature =
        discovered_provider_model_signature("ollama", "qwen3-vl:4b").expect("signature expected");
    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "multimodal");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer, AiBindingPurpose::Vision,]
    );
}

#[test]
fn discovers_ollama_qwen_embedding_model_roles() {
    let signature = discovered_provider_model_signature("ollama", "qwen3-embedding:4b")
        .expect("signature expected");
    assert_eq!(signature.capability_kind, "embedding");
    assert_eq!(signature.modality_kind, "text");
    assert_eq!(signature.allowed_binding_purposes, &[AiBindingPurpose::EmbedChunk]);
}

#[test]
fn bootstrap_preset_inputs_must_cover_all_canonical_purposes() {
    let inputs = vec![
        BootstrapAiPresetInput {
            binding_purpose: AiBindingPurpose::ExtractGraph,
            provider_kind: "openai".to_string(),
            model_catalog_id: Uuid::now_v7(),
            preset_name: "OpenAI Extract Graph · gpt-5.4-nano".to_string(),
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiPresetInput {
            binding_purpose: AiBindingPurpose::EmbedChunk,
            provider_kind: "openai".to_string(),
            model_catalog_id: Uuid::now_v7(),
            preset_name: "OpenAI Embed Chunk · text-embedding-3-large".to_string(),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
        BootstrapAiPresetInput {
            binding_purpose: AiBindingPurpose::QueryAnswer,
            provider_kind: "openai".to_string(),
            model_catalog_id: Uuid::now_v7(),
            preset_name: "OpenAI Query Answer · gpt-5.4-mini".to_string(),
            system_prompt: None,
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        },
    ];

    assert!(!bootstrap_preset_inputs_cover_canonical_purposes(&inputs));
    assert!(matches!(
        validate_bootstrap_preset_inputs_complete(&inputs),
        Err(ApiError::BadRequest(_))
    ));
}

#[test]
fn bootstrap_bundle_uses_expected_openai_models() {
    let provider = sample_provider("openai");
    let extract_graph_model_id = Uuid::now_v7();
    let query_answer_model_id = Uuid::now_v7();
    let embed_model_id = Uuid::now_v7();
    let vision_model_id = Uuid::now_v7();
    let models = vec![
        ModelCatalogEntry {
            id: extract_graph_model_id,
            provider_catalog_id: provider.id,
            model_name: "gpt-5.4-nano".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: query_answer_model_id,
            provider_catalog_id: provider.id,
            model_name: "gpt-5.4-mini".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::QueryAnswer, AiBindingPurpose::Vision],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: embed_model_id,
            provider_catalog_id: provider.id,
            model_name: "text-embedding-3-large".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: vision_model_id,
            provider_catalog_id: provider.id,
            model_name: "gpt-5.4-mini".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::QueryAnswer, AiBindingPurpose::Vision],
            context_window: None,
            max_output_tokens: None,
        },
    ];

    let bundle = resolve_bootstrap_provider_preset_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("openai bundle should resolve")
    .expect("openai bundle should be available");

    assert_eq!(bundle.provider_kind, "openai");
    assert_eq!(bundle.presets.len(), 4);
    assert_eq!(
        bundle
            .presets
            .iter()
            .find(|preset| preset.binding_purpose == AiBindingPurpose::ExtractGraph)
            .map(|preset| preset.model_name.as_str()),
        Some("gpt-5.4-nano")
    );
    assert_eq!(
        bundle
            .presets
            .iter()
            .find(|preset| preset.binding_purpose == AiBindingPurpose::QueryAnswer)
            .and_then(|preset| preset.temperature),
        Some(0.3)
    );
}

#[test]
fn discovered_openai_nano_is_multimodal_and_vision_capable() {
    let signature = discovered_provider_model_signature("openai", "gpt-5.4-nano")
        .expect("gpt-5.4-nano should classify");

    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "multimodal");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer, AiBindingPurpose::Vision,]
    );
}

#[test]
fn discovered_qwen_vl_models_are_multimodal_and_query_capable() {
    let signature = discovered_provider_model_signature("qwen", "qwen-vl-max-latest")
        .expect("qwen-vl-max-latest should classify");

    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "multimodal");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer, AiBindingPurpose::Vision,]
    );
}

#[test]
fn discovered_ollama_embeddings_and_vision_models_classify_correctly() {
    let embedding = discovered_provider_model_signature("ollama", "nomic-embed-text")
        .expect("nomic-embed-text should classify");
    assert_eq!(embedding.capability_kind, "embedding");
    assert_eq!(embedding.modality_kind, "text");
    assert_eq!(embedding.allowed_binding_purposes, &[AiBindingPurpose::EmbedChunk]);

    let vision = discovered_provider_model_signature("ollama", "llama3.2-vision")
        .expect("llama3.2-vision should classify");
    assert_eq!(vision.capability_kind, "chat");
    assert_eq!(vision.modality_kind, "multimodal");
    assert_eq!(
        vision.allowed_binding_purposes,
        &[AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer, AiBindingPurpose::Vision,]
    );
}

#[test]
fn discovered_deepseek_models_remain_text_only() {
    let signature = discovered_provider_model_signature("deepseek", "deepseek-chat")
        .expect("deepseek-chat should classify");

    assert_eq!(signature.capability_kind, "chat");
    assert_eq!(signature.modality_kind, "text");
    assert_eq!(
        signature.allowed_binding_purposes,
        &[AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer]
    );
}

#[test]
fn discovered_openai_non_binding_models_are_skipped() {
    assert!(discovered_provider_model_signature("openai", "gpt-image-1").is_none());
    assert!(discovered_provider_model_signature("openai", "gpt-realtime").is_none());
}

#[test]
fn bootstrap_bundle_uses_expected_ollama_models() {
    let provider = sample_provider("ollama");
    let graph_model_id = Uuid::now_v7();
    let embed_model_id = Uuid::now_v7();
    let vision_model_id = Uuid::now_v7();
    let models = vec![
        ModelCatalogEntry {
            id: graph_model_id,
            provider_catalog_id: provider.id,
            model_name: "qwen3:0.6b".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryAnswer,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: embed_model_id,
            provider_catalog_id: provider.id,
            model_name: "qwen3-embedding:0.6b".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: vision_model_id,
            provider_catalog_id: provider.id,
            model_name: "qwen3-vl:2b".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::Vision],
            context_window: None,
            max_output_tokens: None,
        },
    ];

    let bundle = resolve_bootstrap_provider_preset_bundle(
        &provider,
        std::slice::from_ref(&provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("ollama bundle should resolve")
    .expect("ollama bundle should be available");

    assert_eq!(bundle.provider_kind, "ollama");
    assert_eq!(bundle.default_base_url.as_deref(), Some("https://example.com/v1"));
    assert!(!bundle.api_key_required);
    assert!(bundle.base_url_required);
    assert_eq!(
        bundle
            .presets
            .iter()
            .find(|preset| preset.binding_purpose == AiBindingPurpose::ExtractGraph)
            .map(|preset| preset.model_name.as_str()),
        Some("qwen3:0.6b")
    );
    assert_eq!(
        bundle
            .presets
            .iter()
            .find(|preset| preset.binding_purpose == AiBindingPurpose::Vision)
            .map(|preset| preset.model_name.as_str()),
        Some("qwen3-vl:2b")
    );
}

#[test]
fn canonicalizes_ollama_root_urls_to_v1() {
    let provider = sample_provider("ollama");

    assert_eq!(
        canonicalize_provider_base_url(&provider, "http://localhost:11434")
            .expect("root ollama url should normalize"),
        "http://localhost:11434/v1"
    );
    assert_eq!(
        canonicalize_provider_base_url(&provider, "http://localhost:11434/api")
            .expect("/api ollama url should normalize"),
        "http://localhost:11434/v1"
    );
}

#[test]
fn detects_loopback_base_urls() {
    assert!(is_loopback_base_url("http://localhost:11434/v1"));
    assert!(is_loopback_base_url("http://127.0.0.1:11434/v1"));
    assert!(!is_loopback_base_url("http://host.docker.internal:11434/v1"));
}

#[test]
fn configured_bootstrap_presets_inherit_provider_bundle_tuning_when_models_match() {
    let provider = sample_provider("openai");
    let model = ModelCatalogEntry {
        id: Uuid::now_v7(),
        provider_catalog_id: provider.id,
        model_name: "gpt-5.4-nano".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "multimodal".to_string(),
        allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
        context_window: None,
        max_output_tokens: None,
    };
    let configured = crate::app::config::UiBootstrapAiSetup {
        provider_secrets: vec![crate::app::config::UiBootstrapAiProviderSecret {
            provider_kind: "openai".to_string(),
            api_key: "test-openai-key".to_string(),
        }],
        binding_defaults: vec![UiBootstrapAiBindingDefault {
            binding_purpose: "extract_graph".to_string(),
            provider_kind: Some("openai".to_string()),
            model_name: Some("gpt-5.4-nano".to_string()),
        }],
    };

    let preset_inputs = resolve_configured_bootstrap_preset_inputs(
        &configured,
        std::slice::from_ref(&provider),
        &[model],
    )
    .expect("configured preset inputs should resolve");

    assert_eq!(preset_inputs.len(), 1);
    assert_eq!(preset_inputs[0].provider_kind, "openai");
    assert_eq!(preset_inputs[0].binding_purpose, AiBindingPurpose::ExtractGraph);
    assert_eq!(preset_inputs[0].temperature, Some(0.3));
    assert_eq!(preset_inputs[0].top_p, Some(0.9));
}

#[test]
fn bootstrap_bundle_omits_incomplete_provider_profiles() {
    // DeepSeek's bootstrap profile borrows embed_chunk and vision models
    // from the OpenAI catalog. When the OpenAI provider is not present in
    // the catalog at all, the bundle must be skipped instead of half-built.
    let deepseek_provider = sample_provider("deepseek");
    let models = vec![ModelCatalogEntry {
        id: Uuid::now_v7(),
        provider_catalog_id: deepseek_provider.id,
        model_name: "deepseek-chat".to_string(),
        capability_kind: "chat".to_string(),
        modality_kind: "text".to_string(),
        allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
        context_window: None,
        max_output_tokens: None,
    }];

    let bundle = resolve_bootstrap_provider_preset_bundle(
        &deepseek_provider,
        std::slice::from_ref(&deepseek_provider),
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("deepseek resolution should not error");

    assert!(bundle.is_none());
}

#[test]
fn deepseek_bootstrap_bundle_borrows_openai_for_embed_and_vision() {
    let openai_provider = sample_provider("openai");
    let deepseek_provider = sample_provider("deepseek");
    let providers = vec![deepseek_provider.clone(), openai_provider.clone()];
    let models = vec![
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: deepseek_provider.id,
            model_name: "deepseek-chat".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryAnswer,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: openai_provider.id,
            model_name: "text-embedding-3-large".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: openai_provider.id,
            model_name: "gpt-5.4-mini".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::QueryAnswer, AiBindingPurpose::Vision],
            context_window: None,
            max_output_tokens: None,
        },
    ];

    let bundle = resolve_bootstrap_provider_preset_bundle(
        &deepseek_provider,
        &providers,
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("deepseek bundle should resolve")
    .expect("deepseek bundle should be available");

    let extract = bundle
        .presets
        .iter()
        .find(|preset| preset.binding_purpose == AiBindingPurpose::ExtractGraph)
        .expect("extract_graph preset");
    assert_eq!(extract.owner_provider_kind, "deepseek");
    assert_eq!(extract.owner_provider_catalog_id, deepseek_provider.id);
    assert_eq!(extract.model_name, "deepseek-chat");

    let embed = bundle
        .presets
        .iter()
        .find(|preset| preset.binding_purpose == AiBindingPurpose::EmbedChunk)
        .expect("embed_chunk preset");
    assert_eq!(embed.owner_provider_kind, "openai");
    assert_eq!(embed.owner_provider_catalog_id, openai_provider.id);
    assert_eq!(embed.model_name, "text-embedding-3-large");

    let vision = bundle
        .presets
        .iter()
        .find(|preset| preset.binding_purpose == AiBindingPurpose::Vision)
        .expect("vision preset");
    assert_eq!(vision.owner_provider_kind, "openai");
    assert_eq!(vision.owner_provider_catalog_id, openai_provider.id);
    assert_eq!(vision.model_name, "gpt-5.4-mini");
}

#[test]
fn deepseek_bootstrap_bundle_requires_env_backed_openai_fallback() {
    let openai_provider = sample_provider("openai");
    let deepseek_provider = sample_provider("deepseek");
    let providers = vec![deepseek_provider.clone(), openai_provider.clone()];
    let models = vec![
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: deepseek_provider.id,
            model_name: "deepseek-chat".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            allowed_binding_purposes: vec![
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::QueryAnswer,
            ],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: openai_provider.id,
            model_name: "text-embedding-3-large".to_string(),
            capability_kind: "embedding".to_string(),
            modality_kind: "text".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
            context_window: None,
            max_output_tokens: None,
        },
        ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: openai_provider.id,
            model_name: "gpt-5.4-mini".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::QueryAnswer, AiBindingPurpose::Vision],
            context_window: None,
            max_output_tokens: None,
        },
    ];
    let bundle = resolve_bootstrap_provider_preset_bundle(
        &deepseek_provider,
        &providers,
        &models,
        BootstrapAiCredentialSource::Missing,
    )
    .expect("deepseek bundle should resolve")
    .expect("deepseek bundle should be available");

    assert!(!bootstrap_bundle_has_required_fallback_credentials(&bundle, &providers, None));

    let configured = UiBootstrapAiSetup {
        provider_secrets: vec![UiBootstrapAiProviderSecret {
            provider_kind: "openai".to_string(),
            api_key: "test-openai-key".to_string(),
        }],
        binding_defaults: vec![],
    };
    assert!(bootstrap_bundle_has_required_fallback_credentials(
        &bundle,
        &providers,
        Some(&configured),
    ));
}
