use super::bootstrap::missing_bootstrap_model_list_models;
use super::bootstrap_persistence::bootstrap_binding_update_command;
use super::catalog::{model_available_account_ids, parse_provider_profile};
use super::provider_access::{
    provider_validation_extra_parameters, validate_provider_model_listing,
};
use super::provider_validation::{
    ChatRoundTripValidationFailureKind, ProviderCredentialValidationFailureKind,
    chat_round_trip_validation_failure_kind, discovered_model_from_listing_entry,
    normalize_provider_base_url_input, provider_credential_validation_error,
    provider_credential_validation_failure_kind,
};
use super::{
    ApplyBootstrapProviderBindingBundleCommand, BindingTarget, BootstrapAiBindingInput,
    BootstrapAiCredentialInput, BootstrapAiCredentialSource, CreateAiAccountCommand,
    REQUIRED_BINDING_PURPOSES, ResolvedRuntimeBinding, UpdateAiAccountCommand,
    bootstrap_binding_inputs_cover_required_purposes, bootstrap_bundle_is_self_contained,
    bootstrap_env_credential_needs_sync, deduplicate_binding_purposes, is_loopback_base_url,
    is_provider_credential_validation_error, map_model_row, normalize_provider_base_url,
    provider_credential_base_url_for_create, provider_credential_base_url_for_update,
    resolve_bootstrap_provider_binding_bundle, resolve_bootstrap_provider_binding_descriptors,
    resolve_configured_bootstrap_binding_inputs, resolve_effective_embedding_dimensions,
    runtime_provider_base_url, validate_binding_request_policy, validate_binding_target,
    validate_bootstrap_binding_inputs_cover_required_purposes, validate_model_binding_purpose,
    validate_model_metadata, validate_model_purposes_for_provider,
    validate_provider_base_url_key_reuse,
};
use crate::app::config::UiBootstrapAiBindingDefault;
use crate::domains::ai::{
    AiAccount, AiBinding, AiBindingPurpose, AiScopeKind, ModelCatalogEntry, ProviderCatalogEntry,
};
use crate::domains::provider_profiles::{
    OPENAI_COMPATIBLE_RUNTIME_KIND, ProviderAuthScheme, ProviderBaseUrlMode, ProviderBaseUrlPolicy,
    ProviderCapabilities, ProviderCapabilityState, ProviderCredentialPolicy,
    ProviderCredentialValidationMode, ProviderModelCapabilityKind, ProviderModelDiscovery,
    ProviderModelDiscoveryMode, ProviderModelDiscoveryPath, ProviderProfile, ProviderRequestPolicy,
    ProviderRuntimeProfile, ProviderSamplingPolicy, ProviderStructuredOutputMode,
    ProviderTokenLimitParameter, ProviderToolChoicePolicy, ProviderUsagePolicy,
};
use crate::interfaces::http::router_support::ApiError;
use crate::shared::secret_encryption::{CredentialCipher, SecretPurpose};
use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::binding_affects_vector_index_dimension;
use super::runtime::{
    merge_binding_request_context, merge_model_request_policy, merge_provider_runtime_profile,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use uuid::Uuid;

mod bootstrap_bundles;
mod catalog_and_purposes;
mod decision_eligibility;
mod diagnostics_and_profiles;
mod provider_validation_tests;
mod runtime_resolution;

fn sample_model(allowed_binding_purposes: Vec<AiBindingPurpose>) -> ModelCatalogEntry {
    catalog_model(Uuid::nil(), Uuid::nil(), "sample-model", allowed_binding_purposes)
}

fn catalog_model(
    id: Uuid,
    provider_catalog_id: Uuid,
    model_name: &str,
    allowed_binding_purposes: Vec<AiBindingPurpose>,
) -> ModelCatalogEntry {
    ModelCatalogEntry {
        id,
        provider_catalog_id,
        model_name: model_name.to_string(),
        lifecycle_state: "active".to_string(),
        metadata_json: serde_json::json!({}),
        allowed_binding_purposes,
        context_window: None,
        max_output_tokens: None,
        pricing: free_test_pricing(),
    }
}

fn free_test_pricing() -> crate::domains::ai::ModelPricing {
    crate::domains::ai::ModelPricing {
        currency_code: "USD".to_string(),
        input_price_per_1m: rust_decimal::Decimal::ZERO,
        cached_input_price_per_1m: rust_decimal::Decimal::ZERO,
        cache_write_input_price_per_1m: rust_decimal::Decimal::ZERO,
        output_price_per_1m: rust_decimal::Decimal::ZERO,
        long_context: None,
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
        provider_catalog_id: Uuid::nil(),
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

fn bootstrap_binding_input_for_purpose(
    binding_purpose: AiBindingPurpose,
) -> BootstrapAiBindingInput {
    BootstrapAiBindingInput {
        binding_purpose,
        provider_kind: "provider-alpha".to_string(),
        model_catalog_id: Uuid::now_v7(),
        system_prompt: None,
        temperature: (binding_purpose != AiBindingPurpose::EmbedChunk).then_some(0.3),
        top_p: (binding_purpose != AiBindingPurpose::EmbedChunk).then_some(0.9),
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    }
}

fn sample_model_row(
    lifecycle_state: &str,
    metadata_json: serde_json::Value,
) -> crate::infra::repositories::ai_repository::AiModelCatalogRow {
    crate::infra::repositories::ai_repository::AiModelCatalogRow {
        id: Uuid::nil(),
        provider_catalog_id: Uuid::nil(),
        model_name: "sample-model".to_string(),
        context_window: None,
        max_output_tokens: None,
        lifecycle_state: lifecycle_state.to_string(),
        metadata_json,
        allowed_binding_purposes: vec!["extract_graph".to_string(), "query_answer".to_string()],
        currency_code: "USD".to_string(),
        input_price_per_1m: rust_decimal::Decimal::ZERO,
        cached_input_price_per_1m: rust_decimal::Decimal::ZERO,
        cache_write_input_price_per_1m: rust_decimal::Decimal::ZERO,
        output_price_per_1m: rust_decimal::Decimal::ZERO,
        long_context_threshold_tokens: None,
        long_context_input_price_per_1m: None,
        long_context_cached_input_price_per_1m: None,
        long_context_output_price_per_1m: None,
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
                capability_kind: ProviderModelCapabilityKind::Chat,
                path: "/models".to_string(),
            },
            ProviderModelDiscoveryPath {
                capability_kind: ProviderModelCapabilityKind::Embedding,
                path: "/models".to_string(),
            },
        ],
        listed_pricing_currency: Some("USD".to_string()),
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
        decision: ProviderCapabilityState::Unsupported,
        flex_tier: ProviderCapabilityState::Unsupported,
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
        score_criteria: Vec::new(),
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
        service_tier_multipliers: std::collections::BTreeMap::new(),
        purpose_gaps: Vec::new(),
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
        purpose_gaps: Vec::new(),
        profile,
    }
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

/// Advertise this process's routable interface rather than loopback: a
/// provider URL that allows private networks may be rewritten to the Docker
/// host gateway, so the fixture must stay reachable from inside the executor.
fn advertised_test_host() -> IpAddr {
    let route_probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind route probe");
    route_probe.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).expect("resolve test route");
    match route_probe.local_addr().expect("test route address").ip() {
        address @ IpAddr::V4(_) if !address.is_unspecified() && !address.is_loopback() => address,
        address => panic!("test route must be a non-loopback IPv4, got {address}"),
    }
}

async fn read_full_http_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    loop {
        let mut chunk = [0_u8; 2048];
        let read = socket.read(&mut chunk).await.expect("read test request");
        if read == 0 {
            return request;
        }
        request.extend_from_slice(&chunk[..read]);
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or_default();
        if request.len() >= header_end + 4 + content_length {
            return request;
        }
    }
}

/// A mock provider endpoint. It accepts every request until a short idle
/// timeout proves the caller has stopped, so a test can assert the exact set
/// of requests a validation made — not merely the last one.
async fn recording_http_server(
    status: &'static str,
    body: &'static str,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let host = advertised_test_host();
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.expect("bind test server");
    let port = listener.local_addr().expect("test server address").port();
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        loop {
            let accepted =
                tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
            let Ok(Ok((mut socket, _))) = accepted else { break };
            let request = read_full_http_request(&mut socket).await;
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            socket.write_all(response.as_bytes()).await.expect("write test response");
            requests.push(String::from_utf8_lossy(&request).to_string());
        }
        requests
    });
    (format!("http://{host}:{port}"), handle)
}

/// The TypeSafe Jev provider as account validation sees it, pointed at a local
/// mock: a decision provider whose credential is validated by listing models.
fn typesafe_test_provider(base_url: &str) -> ProviderCatalogEntry {
    let mut provider = sample_provider("provider-alpha");
    provider.provider_kind = "typesafe".to_string();
    provider.display_name = "TypeSafe Jev".to_string();
    provider.api_style = "typesafe_system_one".to_string();
    provider.default_base_url = Some(base_url.to_string());
    provider.credential_policy = ProviderCredentialPolicy {
        api_key_required: true,
        base_url_required: false,
        base_url_mode: ProviderBaseUrlMode::Fixed,
        validation_mode: ProviderCredentialValidationMode::ModelList,
    };
    provider.api_key_required = true;
    provider.base_url_required = false;
    provider.base_url_policy = ProviderBaseUrlPolicy {
        allow_override: false,
        require_https: false,
        allow_private_network: true,
        trim_suffixes: Vec::new(),
    };
    provider.model_discovery = ProviderModelDiscovery {
        mode: ProviderModelDiscoveryMode::Credential,
        paths: vec![ProviderModelDiscoveryPath {
            capability_kind: ProviderModelCapabilityKind::Decision,
            path: "/v1/models".to_string(),
        }],
        listed_pricing_currency: None,
    };
    provider.capabilities = ProviderCapabilities {
        chat: ProviderCapabilityState::Unsupported,
        embeddings: ProviderCapabilityState::Unsupported,
        vision: ProviderCapabilityState::Unsupported,
        streaming: ProviderCapabilityState::Unsupported,
        tools: ProviderCapabilityState::Unsupported,
        model_discovery: ProviderCapabilityState::Supported,
        decision: ProviderCapabilityState::Supported,
        flex_tier: ProviderCapabilityState::Unsupported,
    };
    provider.runtime = ProviderRuntimeProfile {
        kind: "typesafe_system_one".to_string(),
        auth_scheme: ProviderAuthScheme::Bearer,
        token_limit_parameter: ProviderTokenLimitParameter::MaxTokens,
        structured_output: ProviderStructuredOutputMode::Unsupported,
        chat_path: "/v1/systemone".to_string(),
        embeddings_path: None,
        models_path: Some("/v1/models".to_string()),
        score_criteria: vec![
            "not relevant to the query".to_string(),
            "directly relevant to the query".to_string(),
        ],
    };
    provider
}

#[tokio::test]
async fn a_decision_provider_validates_its_credential_by_listing_models_not_chatting() {
    let (base_url, server) =
        recording_http_server("200 OK", r#"{"data":[{"id":"jev-latest"}]}"#).await;
    let provider = typesafe_test_provider(&base_url);
    assert_eq!(
        provider.credential_policy.validation_mode,
        ProviderCredentialValidationMode::ModelList,
        "TypeSafe exposes no chat round trip, so it must validate by model listing"
    );

    validate_provider_model_listing(&provider, Some("secret"), Some(&base_url))
        .await
        .expect("a models listing must validate the credential");

    let requests = server.await.expect("mock server must finish");
    assert_eq!(requests.len(), 1, "credential validation must issue exactly one request");
    assert!(
        requests[0].starts_with("GET /v1/models "),
        "the single request must be GET /v1/models, got: {}",
        requests[0].lines().next().unwrap_or_default()
    );
    assert!(!requests[0].starts_with("POST"), "ModelList validation must not POST");
    assert!(
        !requests[0].contains("/v1/systemone"),
        "ModelList validation must not touch the decision invocation path"
    );
}

#[tokio::test]
async fn a_decision_provider_maps_a_rejected_api_key_to_rejected() {
    let (base_url, server) = recording_http_server("401 Unauthorized", "{}").await;
    let provider = typesafe_test_provider(&base_url);

    let error = validate_provider_model_listing(&provider, Some("secret"), Some(&base_url))
        .await
        .expect_err("a 401 must fail validation");
    assert_eq!(
        provider_credential_validation_failure_kind(&error),
        Some(ProviderCredentialValidationFailureKind::Rejected),
        "a 401 must map to a rejected credential, not a transport failure"
    );

    let requests = server.await.expect("mock server must finish");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /v1/models "));
}

#[test]
fn binding_reasoning_selection_requires_the_exact_model_capability() {
    let selection = serde_json::json!({"reasoningMode": {"version": 1, "mode": "none"}});
    let provider = sample_provider("provider-alpha");
    let mut model = sample_model(vec![AiBindingPurpose::QueryAnswer]);

    // A provider-level grant is not a model grant: a model without its own
    // confirmation rejects the selection.
    let error = super::binding_operations::validate_binding_reasoning_selection(
        &selection, &model, &provider,
    )
    .expect_err("a model without its own confirmation must reject the selection");
    assert!(error.to_string().contains("not confirmed"), "{error}");

    // Confirming the exact model's catalog row accepts the selection.
    model.metadata_json = serde_json::json!({
        "reasoningCapability": {
            "confirmed": {"mode": "none", "wire": "reasoning_effort_none",
                          "evidence": "https://developers.openai.com/api/docs/guides/reasoning"}
        }
    });
    super::binding_operations::validate_binding_reasoning_selection(&selection, &model, &provider)
        .expect("a confirmed model must accept the selected mode");

    // A raw parameter that disagrees with the confirmed wire control is a 4xx.
    let conflicting = serde_json::json!({
        "reasoningMode": {"version": 1, "mode": "none"},
        "reasoning_effort": "high",
    });
    let error = super::binding_operations::validate_binding_reasoning_selection(
        &conflicting,
        &model,
        &provider,
    )
    .expect_err("a raw parameter that disagrees with the wire control must be rejected");
    assert!(error.to_string().contains("disagrees"), "{error}");

    // Another unconfirmed model is not granted by this model's confirmation.
    let other = sample_model(vec![AiBindingPurpose::QueryAnswer]);
    let error = super::binding_operations::validate_binding_reasoning_selection(
        &selection, &other, &provider,
    )
    .expect_err("a different unconfirmed model must reject the selection");
    assert!(error.to_string().contains("not confirmed"), "{error}");

    // An absent selection is the honest unset state and is always allowed.
    super::binding_operations::validate_binding_reasoning_selection(
        &serde_json::json!({}),
        &other,
        &provider,
    )
    .expect("no selection is not an unsupported selection");
}

#[test]
fn merged_binding_context_projects_the_exact_model_capability_over_client_input() {
    let provider = sample_provider("provider-alpha");
    let mut model = sample_model(vec![AiBindingPurpose::QueryAnswer]);
    model.model_name = "alpha-chat-plus".to_string();
    model.metadata_json = serde_json::json!({
        "reasoningCapability": {
            "confirmed": {"mode": "none", "wire": "reasoning_effort_none",
                          "evidence": "https://developers.openai.com/api/docs/guides/reasoning"}
        }
    });

    // A client tries to forge the reserved projection and to smuggle its own
    // capability through its request policy; the server projection must win.
    let client = serde_json::json!({
        "reasoningMode": {"version": 1, "mode": "none"},
        "_providerModelReasoning": {"modelName": "some-other-model", "capability": "unconfirmed"},
        "_providerRequestPolicy": {"sampling": "omit"}
    });
    let merged = merge_binding_request_context(client, &model, &provider.profile);

    assert_eq!(
        merged.pointer("/_providerModelReasoning/modelName").and_then(serde_json::Value::as_str),
        Some("alpha-chat-plus"),
        "the exact bound model identity must be projected"
    );
    assert_eq!(
        merged
            .pointer("/_providerModelReasoning/capability/confirmed/wire")
            .and_then(serde_json::Value::as_str),
        Some("reasoning_effort_none"),
        "the model's own confirmation must be projected"
    );
    assert_eq!(
        merged.pointer("/_providerRequestPolicy/sampling").and_then(serde_json::Value::as_str),
        Some("omit"),
        "a legitimate override of another automatic field survives"
    );
}

#[test]
fn typed_selection_rejects_either_caller_owned_switch_before_runtime_injection() {
    let provider = sample_provider("synthetic-provider");
    let mut model = sample_model(vec![AiBindingPurpose::QueryAnswer]);
    for wire in ["reasoning_effort_none", "thinking_disabled"] {
        model.metadata_json = serde_json::json!({"reasoningCapability": {"confirmed": {
            "mode": "none", "wire": wire, "evidence": "synthetic catalog fixture"
        }}});
        for raw in [
            serde_json::json!({"reasoning_effort": "high"}),
            serde_json::json!({"thinking": {"type": "enabled"}}),
        ] {
            let mut selection = raw.clone();
            selection["reasoningMode"] = serde_json::json!({"version": 1, "mode": "none"});
            let error = super::binding_operations::validate_binding_reasoning_selection(
                &selection, &model, &provider,
            )
            .expect_err("either caller-owned control must conflict");
            assert!(matches!(error, ApiError::BadRequest(_)), "{error}");
            // No selection means legacy raw input is untouched, not rejected.
            super::binding_operations::validate_binding_reasoning_selection(
                &raw, &model, &provider,
            )
            .expect("unset preserves raw behaviour");
        }
    }
}
