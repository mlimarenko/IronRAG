use crate::domains::provider_profiles::{
    ProviderAuthScheme, ProviderSamplingPolicy, ProviderToolChoicePolicy,
};

use super::UnifiedGateway;

fn hosted_openai_provider_profile() -> serde_json::Value {
    serde_json::json!({
        "_providerProfile": {
            "runtime": {
                "kind": "openai_compatible",
                "authScheme": "bearer",
                "tokenLimitParameter": "max_tokens",
                "structuredOutput": "json_schema",
                "chatPath": "/chat/completions",
                "embeddingsPath": null,
                "modelsPath": "/models"
            },
            "baseUrl": {
                "allowOverride": false,
                "requireHttps": true,
                "allowPrivateNetwork": false,
                "trimSuffixes": []
            },
            "credentials": {
                "apiKeyRequired": true,
                "baseUrlRequired": false,
                "baseUrlMode": "fixed",
                "validationMode": "model_list"
            }
        }
    })
}

#[test]
fn allows_ollama_provider_without_api_key() {
    let provider_profile = serde_json::json!({
        "runtime": {
            "kind": "openai_compatible",
            "authScheme": "bearer",
            "tokenLimitParameter": "max_tokens",
            "structuredOutput": "json_schema",
            "chatPath": "/chat/completions",
            "embeddingsPath": "/embeddings",
            "modelsPath": "/models"
        },
        "baseUrl": {
            "allowOverride": true,
            "requireHttps": false,
            "allowPrivateNetwork": true,
            "trimSuffixes": ["/v1"]
        },
        "credentials": {
            "apiKeyRequired": false,
            "baseUrlRequired": true,
            "baseUrlMode": "required",
            "validationMode": "model_list"
        }
    });
    let extra_parameters_json = serde_json::json!({
        "_providerProfile": provider_profile,
    });

    let resolved = UnifiedGateway::resolve_provider(
        "ollama",
        None,
        Some("http://localhost:11434/v1"),
        "synthetic-model",
        &extra_parameters_json,
    )
    .expect("ollama should resolve without token");
    assert!(resolved.api_key.is_none());
    assert!(
        crate::shared::provider_base_url::provider_base_url_candidates(
            true,
            "http://localhost:11434/v1"
        )
        .contains(&resolved.base_url)
    );
}

#[test]
fn resolves_raw_authorization_runtime_profile() {
    let resolved = UnifiedGateway::resolve_provider(
        "synthetic-router",
        Some("plain-secret"),
        Some("https://router.example/v1"),
        "synthetic-model",
        &serde_json::json!({
            "_providerProfile": {
                "runtime": {
                    "kind": "openai_compatible",
                    "authScheme": "raw_authorization",
                    "tokenLimitParameter": "max_tokens",
                    "structuredOutput": "json_schema",
                    "chatPath": "/chat/completions",
                    "embeddingsPath": null,
                    "modelsPath": "/models"
                },
                "baseUrl": {
                    "allowOverride": false,
                    "requireHttps": true,
                    "allowPrivateNetwork": false,
                    "trimSuffixes": []
                },
                "credentials": {
                    "apiKeyRequired": true,
                    "baseUrlRequired": false,
                    "baseUrlMode": "fixed",
                    "validationMode": "model_list"
                },
                "requestPolicy": {
                    "sampling": "omit",
                    "toolChoice": "auto_only",
                    "defaultToolMaxOutputTokens": 1024
                }
            }
        }),
    )
    .expect("raw authorization profile should resolve");
    assert_eq!(resolved.api_key.as_deref(), Some("plain-secret"));
    assert_eq!(resolved.runtime.auth_scheme, ProviderAuthScheme::RawAuthorization);
    assert_eq!(resolved.request_policy.sampling, ProviderSamplingPolicy::Omit);
    assert_eq!(resolved.request_policy.tool_choice, ProviderToolChoicePolicy::AutoOnly);
    assert_eq!(resolved.request_policy.default_tool_max_output_tokens, Some(1024));
    assert_eq!(resolved.base_url, "https://router.example/v1");
}

#[test]
fn runtime_profile_is_required() {
    let error = UnifiedGateway::resolve_provider(
        "synthetic-router",
        Some("plain-secret"),
        Some("https://router.example/v1"),
        "synthetic-model",
        &serde_json::json!({}),
    )
    .expect_err("runtime must be catalog-profile driven");
    assert!(error.to_string().contains("missing runtime provider profile"));
}

#[test]
fn runtime_profile_rejects_incomplete_provider_profile() {
    let error = UnifiedGateway::resolve_provider(
        "synthetic-router",
        Some("plain-secret"),
        Some("https://router.example/v1"),
        "synthetic-model",
        &serde_json::json!({
            "_providerProfile": {
                "runtime": {
                    "kind": "openai_compatible",
                    "authScheme": "bearer"
                }
            }
        }),
    )
    .expect_err("runtime profile must be the full defined shape");

    assert!(error.to_string().contains("invalid runtime provider profile"));
}

#[test]
fn runtime_profile_rejects_unsupported_runtime_kind() {
    let error = UnifiedGateway::resolve_provider(
        "synthetic-router",
        Some("plain-secret"),
        Some("https://router.example/v1"),
        "synthetic-model",
        &serde_json::json!({
            "_providerProfile": {
                "runtime": {
                    "kind": "unsupported_runtime",
                    "authScheme": "bearer",
                    "tokenLimitParameter": "max_tokens",
                    "structuredOutput": "json_schema",
                    "chatPath": "/chat/completions",
                    "embeddingsPath": "/embeddings",
                    "modelsPath": "/models"
                },
                "baseUrl": {
                    "allowOverride": false,
                    "requireHttps": true,
                    "allowPrivateNetwork": false,
                    "trimSuffixes": []
                },
                "credentials": {
                    "apiKeyRequired": true,
                    "baseUrlRequired": false,
                    "baseUrlMode": "fixed",
                    "validationMode": "model_list"
                }
            }
        }),
    )
    .expect_err("runtime kind must stay normalized");

    assert!(error.to_string().contains("unsupported provider runtime kind"));
}

/// IRG-182: the seeded TypeSafe decision provider carries
/// `runtime.kind: typesafe_system_one`, which the generative gateway must
/// refuse outright. IRG-187 dispatches this kind in semantic rerank through the
/// SystemOne client; the chat gateway still never dispatches it, so a decision
/// binding cannot be mis-sent as an OpenAI-compatible chat call to
/// `/v1/systemone`.
#[test]
fn runtime_profile_rejects_the_typesafe_system_one_decision_kind() {
    let error = UnifiedGateway::resolve_provider(
        "typesafe",
        Some("plain-secret"),
        Some("https://api.typesafe.ai"),
        "synthetic-model",
        &serde_json::json!({
            "_providerProfile": {
                "runtime": {
                    "kind": "typesafe_system_one",
                    "authScheme": "bearer",
                    "tokenLimitParameter": "max_tokens",
                    "structuredOutput": "unsupported",
                    "chatPath": "/v1/systemone",
                    "embeddingsPath": null,
                    "modelsPath": "/v1/models"
                },
                "baseUrl": {
                    "allowOverride": false,
                    "requireHttps": true,
                    "allowPrivateNetwork": false,
                    "trimSuffixes": []
                },
                "credentials": {
                    "apiKeyRequired": true,
                    "baseUrlRequired": false,
                    "baseUrlMode": "fixed",
                    "validationMode": "model_list"
                }
            }
        }),
    )
    .expect_err("the generative gateway must not dispatch a decision runtime");

    assert!(error.to_string().contains("unsupported provider runtime kind"));
}

#[test]
fn runtime_profile_rejects_private_hosted_base_url() {
    let error = UnifiedGateway::resolve_provider(
        "synthetic-router",
        Some("plain-secret"),
        Some("https://127.0.0.1/v1"),
        "synthetic-model",
        &hosted_openai_provider_profile(),
    )
    .expect_err("hosted runtime must reject stale private base URLs");
    assert!(error.to_string().contains("private"));
}

#[test]
fn runtime_profile_rejects_non_http_base_url() {
    let error = UnifiedGateway::resolve_provider(
        "synthetic-router",
        Some("plain-secret"),
        Some("file:///tmp/provider.sock"),
        "synthetic-model",
        &hosted_openai_provider_profile(),
    )
    .expect_err("runtime must reject non-http provider URLs");
    assert!(error.to_string().contains("http or https"));
}
