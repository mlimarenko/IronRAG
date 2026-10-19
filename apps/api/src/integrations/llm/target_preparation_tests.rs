//! Every LLM call path that prepares a provider target must surface a failed
//! preparation as the shared typed [`ProviderCallError`], so operator failure
//! classification sees the upstream class of a DNS failure and a URL, address
//! or origin policy rejection stays terminal.

use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    time::Duration,
};

use crate::{
    domains::{
        provider_profiles::{
            ANTHROPIC_MESSAGES_RUNTIME_KIND, OPENAI_COMPATIBLE_RUNTIME_KIND, ProviderAuthScheme,
            ProviderRequestPolicy, ProviderRuntimeProfile, ProviderStructuredOutputMode,
            ProviderTokenLimitParameter,
        },
        runtime_ingestion::RuntimeProviderFailureClass,
    },
    integrations::{
        provider_budget::{
            ProviderBudgetConfig, ProviderBudgetRegistry, ProviderBudgetRegistryOptions,
        },
        retry::RetryPolicy,
    },
    services::ops::provider_failure::ProviderFailureClassificationService,
    shared::provider_http::{
        ProviderHttpError, ProviderHttpTransport, ProviderHttpTransportConfig,
    },
};

use super::{
    UnifiedGateway,
    chat::NativeChatCall,
    openai_compatible::{
        OpenAiCompatibleMessage, OpenAiCompatibleMessageContent, OpenAiCompatibleRequest,
    },
    provider::ResolvedProviderRuntime,
};

/// A host name, so preparation always consults the injected resolver.
const UNRESOLVABLE_BASE_URL: &str = "https://provider.invalid/v1";

fn gateway_whose_resolution_fails_with(error: ProviderHttpError) -> UnifiedGateway {
    UnifiedGateway {
        transport: Arc::new(
            ProviderHttpTransport::try_new_with_failing_resolver(
                ProviderHttpTransportConfig::llm(Duration::from_secs(2)),
                error,
            )
            .expect("failing-resolver transport"),
        ),
        provider_budget: Arc::new(
            ProviderBudgetRegistry::uniform(
                ProviderBudgetConfig::unlimited(),
                ProviderBudgetRegistryOptions::default(),
            )
            .expect("test provider budget"),
        ),
        retry_policy: RetryPolicy::no_retry(),
    }
}

fn runtime_profile(kind: &str) -> ProviderRuntimeProfile {
    ProviderRuntimeProfile {
        kind: kind.to_string(),
        auth_scheme: ProviderAuthScheme::Bearer,
        token_limit_parameter: ProviderTokenLimitParameter::MaxTokens,
        structured_output: ProviderStructuredOutputMode::Unsupported,
        chat_path: "/chat/completions".to_string(),
        embeddings_path: Some("/embeddings".to_string()),
        models_path: Some("/models".to_string()),
        score_criteria: Vec::new(),
    }
}

fn openai_request(extra_parameters_json: &serde_json::Value) -> OpenAiCompatibleRequest<'_> {
    OpenAiCompatibleRequest {
        provider_kind: "openai",
        api_key: Some("synthetic-api-key"),
        base_url: UNRESOLVABLE_BASE_URL,
        auth_scheme: ProviderAuthScheme::Bearer,
        chat_path: "/chat/completions".to_string(),
        model_name: "model-alpha",
        messages: vec![OpenAiCompatibleMessage {
            role: "user".to_string(),
            content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
        }],
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        token_limit_parameter: ProviderTokenLimitParameter::MaxCompletionTokens,
        response_format: None,
        extra_parameters_json,
        stream: false,
    }
}

/// Drives every LLM path that prepares a provider target and returns each
/// preparation failure labelled by its call site.
async fn every_prepare_call_site_error(
    error: ProviderHttpError,
) -> Vec<(&'static str, anyhow::Error)> {
    let gateway = gateway_whose_resolution_fails_with(error);
    let resolved = ResolvedProviderRuntime {
        api_key: None,
        base_url: UNRESOLVABLE_BASE_URL.to_string(),
        allow_private_network: false,
        runtime: runtime_profile(OPENAI_COMPATIBLE_RUNTIME_KIND),
        request_policy: ProviderRequestPolicy::default(),
        reasoning_wire: None,
    };
    let native_runtime = runtime_profile(ANTHROPIC_MESSAGES_RUNTIME_KIND);
    let extra_parameters_json = serde_json::json!({});
    let mut failures = Vec::new();

    failures.push((
        "open_embeddings_endpoint",
        gateway.open_embeddings_endpoint("openai", &resolved).await.expect_err("embeddings"),
    ));
    failures.push((
        "open_chat_endpoint",
        gateway.open_chat_endpoint("openai", &resolved).await.expect_err("chat endpoint"),
    ));
    failures.push((
        "call_native_chat",
        gateway
            .call_native_chat(NativeChatCall {
                provider_kind: "anthropic",
                runtime: &native_runtime,
                api_key: Some("synthetic-api-key"),
                base_url: UNRESOLVABLE_BASE_URL,
                allow_private_network: false,
                model_name: "model-alpha",
                prompt: "prompt",
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_output_tokens: Some(16),
                image: None,
            })
            .await
            .expect_err("native chat"),
    ));
    failures.push((
        "call_openai_compatible",
        gateway
            .call_openai_compatible(openai_request(&extra_parameters_json), false)
            .await
            .expect_err("openai-compatible chat"),
    ));
    failures.push((
        "call_openai_compatible_stream",
        gateway
            .call_openai_compatible_stream(
                openai_request(&extra_parameters_json),
                false,
                &mut |_delta: String| {},
            )
            .await
            .expect_err("openai-compatible stream"),
    ));
    failures
}

#[tokio::test]
async fn every_llm_target_preparation_keeps_the_typed_dns_failure_class() {
    let service = ProviderFailureClassificationService::default();
    for (error, expected) in [
        (ProviderHttpError::ResolveTimedOut, RuntimeProviderFailureClass::UpstreamTimeout),
        (ProviderHttpError::ResolveFailed, RuntimeProviderFailureClass::UpstreamProtocolFailure),
        (ProviderHttpError::NoAddresses, RuntimeProviderFailureClass::UpstreamProtocolFailure),
    ] {
        for (site, failure) in every_prepare_call_site_error(error.clone()).await {
            assert_eq!(
                service.classify_error(&failure).as_ref(),
                Some(&expected),
                "{site} must keep the upstream class of {error}: {failure:#}"
            );
            assert!(
                service.is_transient_retryable_error(&failure),
                "{site} must report {error} as transient-retryable: {failure:#}"
            );
        }
    }
}

#[tokio::test]
async fn every_llm_target_policy_rejection_stays_terminal() {
    let service = ProviderFailureClassificationService::default();
    let error = ProviderHttpError::NonPublicAddress(IpAddr::V4(Ipv4Addr::LOCALHOST));
    for (site, failure) in every_prepare_call_site_error(error.clone()).await {
        assert_eq!(
            service.classify_error(&failure),
            Some(RuntimeProviderFailureClass::InternalRequestInvalid),
            "{site} must keep {error} a terminal policy rejection: {failure:#}"
        );
        assert!(
            !service.is_transient_retryable_error(&failure),
            "{site} must never retry {error}: {failure:#}"
        );
    }
}
