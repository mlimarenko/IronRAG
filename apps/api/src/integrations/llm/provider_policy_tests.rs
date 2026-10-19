use crate::domains::provider_profiles::{
    ProviderReasoningPolicy, ProviderRequestPolicy, ProviderSamplingPolicy,
    ProviderStructuredOutputMode, ProviderTokenLimitParameter, ProviderToolChoicePolicy,
    ReasoningWireControl,
};

use super::{
    ChatToolDef, UnifiedGateway,
    openai_compatible::{
        OpenAiCompatibleMessage, OpenAiCompatibleMessageContent, OpenAiCompatibleToolDef,
        OpenAiCompatibleToolUseChatRequest, openai_compatible_token_limit_fields,
    },
    provider::{
        binding_request_policy, disable_bounded_answer_reasoning, resolve_provider_request_policy,
    },
    request::{
        OpenAiCompatibleRequestOptions, openai_compatible_request, provider_response_format,
        provider_system_prompt,
    },
};

fn normalized_tool_request_projection(
    provider_kind: &str,
    model_name: &str,
    extra_parameters_json: &serde_json::Value,
) -> serde_json::Value {
    let policy = resolve_provider_request_policy(
        provider_kind,
        extra_parameters_json,
        ProviderRequestPolicy::default(),
    )
    .expect("synthetic request policy should resolve");
    let (temperature, top_p) = policy.sampling_params(Some(0.3), Some(0.9));
    let max_output_tokens = policy.tool_max_output_tokens(None);
    let (max_completion_tokens, max_tokens) = openai_compatible_token_limit_fields(
        ProviderTokenLimitParameter::MaxTokens,
        max_output_tokens,
    );
    let payload = OpenAiCompatibleToolUseChatRequest {
        model: model_name,
        messages: vec![],
        tools: vec![OpenAiCompatibleToolDef::from(&ChatToolDef {
            name: "lookup".to_string(),
            description: "Lookup structured facts".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        })],
        temperature,
        top_p,
        max_completion_tokens,
        max_tokens,
        tool_choice: policy.tool_choice(true, true),
        stream: false,
        extra: UnifiedGateway::upstream_extra_parameters(extra_parameters_json, None),
    };
    let mut projection =
        serde_json::to_value(payload).expect("tool-use request should serialize to JSON");
    projection.as_object_mut().expect("tool-use request should be an object").remove("model");
    projection
}

#[test]
fn unseen_provider_and_model_names_share_standard_request_behavior() {
    let first = normalized_tool_request_projection(
        "provider-nebula",
        "model-orbit",
        &serde_json::json!({}),
    );
    let second = normalized_tool_request_projection(
        "provider-quartz",
        "model-vector",
        &serde_json::json!({}),
    );

    assert_eq!(first, second);
    assert_eq!(first.get("temperature").and_then(serde_json::Value::as_f64), Some(0.3));
    assert_eq!(first.get("top_p").and_then(serde_json::Value::as_f64), Some(0.9));
    assert_eq!(first.get("tool_choice").and_then(serde_json::Value::as_str), Some("required"));
    assert!(first.get("max_tokens").is_none());
}

#[test]
fn explicit_typed_policy_changes_request_behavior() {
    let projection = normalized_tool_request_projection(
        "provider-nebula",
        "model-orbit",
        &serde_json::json!({
            "_providerRequestPolicy": {
                "sampling": "omit",
                "toolChoice": "auto_only",
                "defaultToolMaxOutputTokens": 2048
            }
        }),
    );

    assert!(projection.get("temperature").is_none());
    assert!(projection.get("top_p").is_none());
    assert_eq!(projection.get("tool_choice").and_then(serde_json::Value::as_str), Some("auto"));
    assert_eq!(projection.get("max_tokens").and_then(serde_json::Value::as_i64), Some(2048));
}

#[test]
fn partial_explicit_policy_uses_standard_field_defaults() {
    let resolved = resolve_provider_request_policy(
        "provider-nebula",
        &serde_json::json!({
            "_providerRequestPolicy": {"sampling": "omit"}
        }),
        ProviderRequestPolicy::default(),
    )
    .expect("partial binding policy should resolve with serde defaults");

    assert_eq!(resolved.sampling, ProviderSamplingPolicy::Omit);
    assert_eq!(resolved.tool_choice, ProviderToolChoicePolicy::RequiredCapable);
    assert_eq!(resolved.default_tool_max_output_tokens, None);
}

#[test]
fn binding_policy_has_explicit_precedence_over_profile_policy() {
    let profile_policy = ProviderRequestPolicy {
        sampling: ProviderSamplingPolicy::Omit,
        tool_choice: ProviderToolChoicePolicy::AutoOnly,
        default_tool_max_output_tokens: Some(1024),
        ..ProviderRequestPolicy::default()
    };
    let resolved = resolve_provider_request_policy(
        "provider-nebula",
        &serde_json::json!({
            "_providerRequestPolicy": {
                "sampling": "forward",
                "toolChoice": "required_capable",
                "defaultToolMaxOutputTokens": 4096
            }
        }),
        profile_policy,
    )
    .expect("binding policy should resolve");

    assert_eq!(resolved.sampling, ProviderSamplingPolicy::Forward);
    assert_eq!(resolved.tool_choice, ProviderToolChoicePolicy::RequiredCapable);
    assert_eq!(resolved.default_tool_max_output_tokens, Some(4096));
}

#[test]
fn invalid_explicit_policy_fails_closed() {
    let unknown_mode = resolve_provider_request_policy(
        "provider-nebula",
        &serde_json::json!({
            "_providerRequestPolicy": {"sampling": "guess_from_model_name"}
        }),
        ProviderRequestPolicy::default(),
    )
    .expect_err("unknown policy variants must not be ignored");
    assert!(unknown_mode.to_string().contains("invalid provider request policy"));

    let invalid_limit = resolve_provider_request_policy(
        "provider-nebula",
        &serde_json::json!({
            "_providerRequestPolicy": {"defaultToolMaxOutputTokens": 0}
        }),
        ProviderRequestPolicy::default(),
    )
    .expect_err("non-positive default token limits must fail closed");
    assert!(invalid_limit.to_string().contains("greater than zero"));
}

#[test]
fn internal_request_policy_is_never_forwarded_upstream() {
    let upstream = UnifiedGateway::upstream_extra_parameters(
        &serde_json::json!({
            "_providerRequestPolicy": {"sampling": "omit"},
            "_providerProfile": {"requestPolicy": {"toolChoice": "auto_only"}},
            "vendor_option": true
        }),
        None,
    );

    assert!(upstream.get("_providerRequestPolicy").is_none());
    assert!(upstream.get("_providerProfile").is_none());
    assert_eq!(upstream.get("vendor_option").and_then(serde_json::Value::as_bool), Some(true));
}

#[test]
fn typed_reasoning_policy_controls_bounded_answer_reasoning() {
    let mut extra = serde_json::json!({
        "_providerProfile": {
            "requestPolicy": {"reasoning": "disable_with_none"}
        },
        "reasoning_effort": "high",
        "vendor_option": true,
    });

    assert!(
        disable_bounded_answer_reasoning("provider-nebula", &mut extra),
        "the typed policy must disable reasoning without inspecting the provider name"
    );
    assert_eq!(extra.get("reasoning_effort"), Some(&serde_json::json!("none")));
    assert_eq!(
        extra.get("vendor_option"),
        Some(&serde_json::json!(true)),
        "unrelated binding parameters must survive the reasoning override"
    );
}

#[test]
fn typed_reasoning_policy_defaults_to_forwarding() {
    let mut extra = serde_json::json!({"reasoning_effort": "high"});
    assert!(
        !disable_bounded_answer_reasoning("openai", &mut extra),
        "a provider identity without a typed control must not imply the capability"
    );
    assert_eq!(extra.get("reasoning_effort"), Some(&serde_json::json!("high")));

    let mut no_parameters = serde_json::Value::Null;
    assert!(!disable_bounded_answer_reasoning("openai", &mut no_parameters));
}

#[test]
fn malformed_provider_reasoning_policy_fails_safe() {
    let extra = serde_json::json!({
        "_providerProfile": {"requestPolicy": {"reasoning": "bogus"}}
    });
    let policy = binding_request_policy("provider-nebula", &extra);
    assert!(
        !policy.disables_hidden_reasoning(),
        "an unparseable policy must not claim a reasoning control"
    );

    let mut extra = serde_json::json!({
        "_providerProfile": {"requestPolicy": {"reasoning": "bogus"}},
        "reasoning_effort": "high"
    });
    assert!(!disable_bounded_answer_reasoning("provider-nebula", &mut extra));
    assert_eq!(extra.get("reasoning_effort"), Some(&serde_json::json!("high")));
}

#[test]
fn binding_request_policy_reads_the_provider_profile() {
    let policy = binding_request_policy(
        "provider-nebula",
        &serde_json::json!({
            "_providerProfile": {
                "requestPolicy": {"sampling": "omit", "reasoning": "disable_with_none"}
            }
        }),
    );

    assert!(policy.disables_hidden_reasoning());
    assert_eq!(policy.reasoning_effort_override(), Some("none"));
    assert_eq!(policy.sampling, ProviderSamplingPolicy::Omit);
    assert_eq!(policy.reasoning, ProviderReasoningPolicy::DisableWithNone);

    let overridden = binding_request_policy(
        "provider-nebula",
        &serde_json::json!({
            "_providerProfile": {"requestPolicy": {"reasoning": "disable_with_none"}},
            "_providerRequestPolicy": {"reasoning": "forward"}
        }),
    );
    assert!(
        !overridden.disables_hidden_reasoning(),
        "the model-level request policy has the same precedence as on the provider-call path"
    );
}

#[test]
fn standard_policy_omits_choice_without_tools_and_honors_explicit_max_tokens() {
    let policy = ProviderRequestPolicy::default();

    assert_eq!(policy.tool_choice(false, true), None);
    assert_eq!(policy.tool_max_output_tokens(Some(512)), Some(512));
}

#[test]
fn json_object_runtime_lowers_structured_response_format() {
    let requested = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "query_ir",
            "strict": true,
            "schema": {"type": "object"}
        }
    });
    let lowered = provider_response_format(
        "provider-alpha",
        Some(&requested),
        ProviderStructuredOutputMode::JsonObject,
    )
    .expect("json_object providers should lower structured output")
    .expect("requested structured output should remain present");

    assert_eq!(lowered.get("type").and_then(serde_json::Value::as_str), Some("json_object"));
    assert!(lowered.get("json_schema").is_none());
}

#[test]
fn json_schema_runtime_keeps_structured_system_prompt_unchanged() {
    let requested = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "query_ir",
            "strict": true,
            "schema": {"type": "object", "properties": {"target_entities": {"type": "array"}}}
        }
    });
    let system_prompt = provider_system_prompt(
        "provider-alpha",
        Some("Base compiler prompt"),
        Some(&requested),
        ProviderStructuredOutputMode::JsonSchema,
    )
    .expect("json_schema prompt should remain valid")
    .expect("prompt should remain present");

    assert_eq!(system_prompt, "Base compiler prompt");
}

#[test]
fn json_object_runtime_injects_schema_into_system_prompt() {
    let requested = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "query_ir",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "target_entities": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": {"type": "string"},
                                "role": {"type": "string"}
                            }
                        }
                    }
                }
            }
        }
    });
    let system_prompt = provider_system_prompt(
        "provider-alpha",
        Some("Base compiler prompt"),
        Some(&requested),
        ProviderStructuredOutputMode::JsonObject,
    )
    .expect("json_object prompt should be built")
    .expect("prompt should remain present");

    assert!(system_prompt.starts_with("Base compiler prompt\n\n"));
    assert!(system_prompt.contains("JSON Schema:"));
    assert!(system_prompt.contains("\"target_entities\""));
    assert!(system_prompt.contains("\"label\""));
    assert!(system_prompt.contains("\"role\""));
}

#[test]
fn prompt_only_json_object_runtime_omits_response_format() {
    let requested = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "query_ir",
            "strict": true,
            "schema": {"type": "object"}
        }
    });
    let lowered = provider_response_format(
        "provider-alpha",
        Some(&requested),
        ProviderStructuredOutputMode::PromptOnlyJsonObject,
    )
    .expect("prompt-only providers should accept structured output by prompt contract");

    assert!(lowered.is_none());
}

#[test]
fn prompt_only_json_object_runtime_injects_schema_into_system_prompt() {
    let requested = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "query_ir",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "target_entities": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": {"type": "string"}
                            }
                        }
                    }
                }
            }
        }
    });
    let system_prompt = provider_system_prompt(
        "provider-alpha",
        Some("Base compiler prompt"),
        Some(&requested),
        ProviderStructuredOutputMode::PromptOnlyJsonObject,
    )
    .expect("prompt-only prompt should be built")
    .expect("prompt should remain present");

    assert!(system_prompt.starts_with("Base compiler prompt\n\n"));
    assert!(system_prompt.contains("JSON Schema:"));
    assert!(system_prompt.contains("\"target_entities\""));
    assert!(system_prompt.contains("\"label\""));
}

#[test]
fn json_object_runtime_requires_normalized_schema_for_prompt_injection() {
    let requested = serde_json::json!({"type": "json_schema"});
    let error = provider_system_prompt(
        "provider-alpha",
        Some("Base compiler prompt"),
        Some(&requested),
        ProviderStructuredOutputMode::JsonObject,
    )
    .expect_err("json_object structured output without schema must fail loud");

    assert!(error.to_string().contains("requires a JSON schema"));
}

#[test]
fn unsupported_structured_output_fails_loud() {
    let requested = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "query_ir",
            "strict": true,
            "schema": {"type": "object"}
        }
    });
    let error = provider_response_format(
        "unsupported-provider",
        Some(&requested),
        ProviderStructuredOutputMode::Unsupported,
    )
    .expect_err("unsupported structured output must fail loud");

    assert!(error.to_string().contains("does not support required structured output"));
}

#[test]
fn binding_policy_reads_synthetic_tool_history_delivery() {
    let profile = serde_json::json!({
        "_providerProfile": { "requestPolicy": { "syntheticToolHistory": "inline" } }
    });
    assert!(binding_request_policy("deepseek", &profile).inlines_synthetic_tool_history());

    // The endpoint capability is not negotiable by model metadata: an explicit
    // envelope without the bit must not downgrade the endpoint to a shape it
    // rejects.
    let explicit_envelope = serde_json::json!({
        "_providerProfile": { "requestPolicy": { "syntheticToolHistory": "inline" } },
        "_providerRequestPolicy": { "sampling": "forward" }
    });
    assert!(
        binding_request_policy("deepseek", &explicit_envelope).inlines_synthetic_tool_history()
    );

    // Without the provider profile the default transcript shape stays.
    let default_policy = serde_json::json!({});
    assert!(!binding_request_policy("openai", &default_policy).inlines_synthetic_tool_history());
}

// ---------------------------------------------------------------------------
// IRG-153: typed binding reasoning mode -> exact pair capability -> wire form
// ---------------------------------------------------------------------------

/// A merged binding envelope for an OpenAI-compatible pair whose catalog
/// policy has explicitly confirmed the `none` mode with recorded evidence, and
/// which selects that mode. `service_tier` is an unrelated foreign parameter
/// that must survive.
fn projected_extra(
    token_limit_parameter: &str,
    structured_output: &str,
    model_name: &str,
    capability: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "_providerProfile": {
            "runtime": {
                "kind": "openai_compatible",
                "authScheme": "bearer",
                "tokenLimitParameter": token_limit_parameter,
                "structuredOutput": structured_output,
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
        },
        "_providerModelReasoning": {"modelName": model_name, "capability": capability},
        "reasoningMode": {"version": 1, "mode": "none"},
        "service_tier": "flex",
    })
}

fn confirmed(wire: &str, evidence: &str) -> serde_json::Value {
    // Synthetic model metadata: the source documents the wire vocabulary,
    // not acceptance by any named deployed model. No provider is called here.
    serde_json::json!({"confirmed": {"mode": "none", "wire": wire, "evidence": evidence}})
}

fn tool_request_body(
    provider_kind: &str,
    model_name: &str,
    extra: &serde_json::Value,
    expected: ReasoningWireControl,
) -> serde_json::Value {
    let resolved = UnifiedGateway::resolve_provider(
        provider_kind,
        Some("synthetic-api-key"),
        Some("https://api.example.com/v1"),
        model_name,
        extra,
    )
    .expect("a confirmed model with a selected mode must resolve");
    assert_eq!(resolved.reasoning_wire, Some(expected));
    let upstream = UnifiedGateway::upstream_extra_parameters(extra, resolved.reasoning_wire);
    assert!(
        upstream.get("reasoningMode").is_none(),
        "the typed envelope must never reach the provider"
    );
    assert!(
        upstream.get("_providerModelReasoning").is_none(),
        "the projected capability must never reach the provider"
    );
    assert_eq!(upstream.get("service_tier"), Some(&serde_json::json!("flex")));
    let request = openai_compatible_request(
        provider_kind,
        model_name,
        vec![OpenAiCompatibleMessage {
            role: "user".to_string(),
            content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
        }],
        &resolved,
        OpenAiCompatibleRequestOptions {
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            response_format: None,
            extra_parameters_json: &upstream,
            stream: false,
        },
    );
    let body = request.body().expect("request body must serialize");
    serde_json::from_slice(&body).expect("serialized body must stay valid json")
}

#[test]
fn synthetic_openai_pair_emits_reasoning_effort_and_never_leaks_the_envelope() {
    let extra = projected_extra(
        "max_completion_tokens",
        "json_schema",
        "synthetic-effort-model",
        confirmed(
            "reasoning_effort_none",
            "https://developers.openai.com/api/docs/guides/reasoning",
        ),
    );
    let body = tool_request_body(
        "openai",
        "synthetic-effort-model",
        &extra,
        ReasoningWireControl::ReasoningEffortNone,
    );
    assert_eq!(body.get("reasoning_effort"), Some(&serde_json::json!("none")));
    assert!(body.get("reasoningMode").is_none(), "the envelope must not reach the wire");
    assert_eq!(body.get("service_tier"), Some(&serde_json::json!("flex")));
}

#[test]
fn synthetic_deepseek_pair_emits_thinking_disabled_without_double_sending() {
    // Synthetic catalog confirmation exercises the documented wire shape, not
    // vendor acceptance of a particular deployed model or a paid provider call.
    let mut extra = projected_extra(
        "max_tokens",
        "json_object",
        "synthetic-thinking-model",
        confirmed(
            "thinking_disabled",
            "synthetic fixture based on https://api-docs.deepseek.com/api/create-chat-completion",
        ),
    );
    // The existing automatic policy may have inserted reasoning_effort for a
    // bounded call; the declared thinking control must replace it, not ride
    // beside it.
    extra["reasoning_effort"] = serde_json::json!("none");
    let body = tool_request_body(
        "deepseek",
        "synthetic-thinking-model",
        &extra,
        ReasoningWireControl::ThinkingDisabled,
    );
    assert_eq!(body.get("thinking"), Some(&serde_json::json!({"type": "disabled"})));
    assert!(
        body.get("reasoning_effort").is_none(),
        "a thinking control and reasoning_effort must not be sent together"
    );
    assert!(body.get("reasoningMode").is_none());
}

#[test]
fn absent_selection_preserves_the_automatic_default_and_raw_parameters() {
    let mut extra = projected_extra(
        "max_completion_tokens",
        "json_schema",
        "synthetic-effort-model",
        confirmed(
            "reasoning_effort_none",
            "https://developers.openai.com/api/docs/guides/reasoning",
        ),
    );
    extra.as_object_mut().expect("object").remove("reasoningMode");
    extra["reasoning_effort"] = serde_json::json!("none");
    let resolved = UnifiedGateway::resolve_provider(
        "openai",
        Some("synthetic-api-key"),
        Some("https://api.example.com/v1"),
        "synthetic-effort-model",
        &extra,
    )
    .expect("an unset selection must resolve");
    assert_eq!(resolved.reasoning_wire, None);
    let upstream = UnifiedGateway::upstream_extra_parameters(&extra, resolved.reasoning_wire);
    assert_eq!(
        upstream.get("reasoning_effort"),
        Some(&serde_json::json!("none")),
        "the automatic/raw parameter must survive an unset selection"
    );
}

#[test]
fn client_binding_cannot_self_grant_a_reasoning_capability() {
    // A client that stuffs the pair capability into its own request policy is
    // rejected: the field is model-owned, so its presence is an unknown field.
    let extra = serde_json::json!({
        "_providerProfile": {
            "runtime": {
                "kind": "openai_compatible",
                "authScheme": "bearer",
                "tokenLimitParameter": "max_completion_tokens",
                "structuredOutput": "json_schema",
                "chatPath": "/chat/completions",
                "embeddingsPath": null,
                "modelsPath": "/models"
            },
            "baseUrl": {"allowOverride": false, "requireHttps": true, "allowPrivateNetwork": false, "trimSuffixes": []},
            "credentials": {"apiKeyRequired": true, "baseUrlRequired": false, "baseUrlMode": "fixed", "validationMode": "model_list"}
        },
        "_providerRequestPolicy": {
            "reasoningCapability": {"confirmed": {"mode": "none", "wire": "reasoning_effort_none", "evidence": "self"}}
        },
        "reasoningMode": {"version": 1, "mode": "none"}
    });
    let error = UnifiedGateway::resolve_provider(
        "openai",
        Some("synthetic-api-key"),
        Some("https://api.example.com/v1"),
        "synthetic-effort-model",
        &extra,
    )
    .expect_err("a client-supplied capability must not resolve");
    assert!(error.to_string().contains("invalid provider request policy"), "{error}");
}

#[test]
fn a_confirmed_model_cannot_grant_a_different_model() {
    let extra = projected_extra(
        "max_completion_tokens",
        "json_schema",
        "synthetic-effort-model",
        confirmed(
            "reasoning_effort_none",
            "https://developers.openai.com/api/docs/guides/reasoning",
        ),
    );
    let error = UnifiedGateway::resolve_provider(
        "openai",
        Some("synthetic-api-key"),
        Some("https://api.example.com/v1"),
        "gpt-6-astra",
        &extra,
    )
    .expect_err("a capability projected for another model must not apply");
    assert!(error.to_string().contains("not `gpt-6-astra`"), "{error}");
}

#[test]
fn unconfirmed_or_invalid_stored_capability_refuses_a_selected_mode() {
    for capability in [
        serde_json::json!("unconfirmed"),
        confirmed("reasoning_effort_none", "   "),
        serde_json::json!({"confirmed": {"mode": "none", "wire": "guess_from_name", "evidence": "docs"}}),
    ] {
        let extra = projected_extra(
            "max_completion_tokens",
            "json_schema",
            "synthetic-effort-model",
            capability.clone(),
        );
        let error = UnifiedGateway::resolve_provider(
            "openai",
            Some("synthetic-api-key"),
            Some("https://api.example.com/v1"),
            "synthetic-effort-model",
            &extra,
        )
        .expect_err("an unconfirmed or invalid capability must refuse the selection");
        assert!(
            error.to_string().contains("capability") || error.to_string().contains("confirmed"),
            "{capability}: {error}"
        );
    }
}
