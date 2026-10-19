use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use reqwest::{
    Method, Url,
    header::{ACCEPT, CONTENT_TYPE},
};

use crate::{
    app::config::Settings,
    domains::provider_profiles::ProviderAuthScheme,
    integrations::{
        provider_budget::{
            ProviderBudgetConfig, ProviderBudgetGuard, ProviderBudgetRegistry,
            ProviderBudgetRegistryOptions, ProviderIdentity,
        },
        retry::{ProviderCallError, RetryPolicy, acquire_provider_budget},
    },
    shared::provider_http::{
        PreparedProviderTarget, ProviderHttpTransport, ProviderHttpTransportConfig,
        send_provider_http_request,
    },
};

use super::provider::{ResolvedProviderRuntime, apply_provider_auth, provider_endpoint_url};

#[derive(Clone)]
pub struct UnifiedGateway {
    pub(super) transport: Arc<ProviderHttpTransport>,
    pub(super) provider_budget: Arc<ProviderBudgetRegistry>,
    pub(super) retry_policy: RetryPolicy,
}

impl UnifiedGateway {
    /// Builds the provider gateway with a fail-closed HTTP transport.
    ///
    /// # Errors
    /// Returns an error when the bounded no-redirect client cannot be built.
    pub fn from_settings(settings: &Settings) -> Result<Self> {
        let timeout = Duration::from_secs(settings.llm_http_timeout_seconds.max(1));
        // Aggressive pool / keep-alive tuning to defeat a 90-second
        // stale-connection hang observed on an SBP turn:
        // a `generate_with_tools` POST that direct-curl completed in
        // ~8.5 s spent 102 s inside the gateway. Root cause was a dead
        // HTTP keep-alive socket waiting on the OS TCP timeout (~90 s)
        // before reqwest reissued the request. `pool_idle_timeout`
        // drops cached sockets aggressively; `tcp_keepalive` lets the
        // kernel probe sockets every 15 s so dead peers (load
        // balancer reaping, NAT-table eviction, provider scaling)
        // surface long before they cost a full turn budget. None of
        // these settings change the configured timeout semantics —
        // the gateway still aborts after `llm_http_timeout_seconds`
        // — they only prevent the path from blocking on a known-dead
        // socket.
        let transport = ProviderHttpTransport::try_new(ProviderHttpTransportConfig::llm(timeout))
            .context("failed to build provider HTTP transport")?;
        let provider_budget = ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig {
                max_outbound: settings.provider_concurrency_max_outbound,
                query_reserved: settings.provider_concurrency_query_reserved,
                requests_per_second: settings.provider_rate_limit_requests_per_second,
                burst: settings.provider_rate_limit_burst,
            },
            ProviderBudgetRegistryOptions {
                acquire_timeout: Duration::from_millis(
                    settings.provider_concurrency_acquire_timeout_ms,
                ),
                max_entries: settings.provider_concurrency_registry_max_entries,
                idle_ttl: Duration::from_secs(
                    settings.provider_concurrency_registry_idle_ttl_seconds,
                ),
            },
        )?;
        let retry_policy = RetryPolicy::idempotent_provider_call(
            settings.provider_retry_max_attempts,
            Duration::from_millis(settings.provider_retry_initial_backoff_ms),
            Duration::from_millis(settings.provider_retry_max_backoff_ms),
        );
        Ok(Self {
            transport: Arc::new(transport),
            provider_budget: Arc::new(provider_budget),
            retry_policy,
        })
    }

    /// The provider concurrency/rate registry every gateway-dispatched attempt
    /// draws from.
    ///
    /// Handed to the TypeSafe SystemOne decision client (IRG-187) so a decision
    /// call to the same physical provider endpoint shares the one limiter
    /// instead of opening a second, unbounded outbound path that could starve
    /// or overload the endpoint behind the gateway's back.
    #[must_use]
    pub fn provider_budget(&self) -> Arc<ProviderBudgetRegistry> {
        Arc::clone(&self.provider_budget)
    }

    /// Same as [`Self::open_chat_endpoint`] for the embeddings endpoint, which
    /// providers may not offer at all.
    pub(super) async fn open_embeddings_endpoint(
        &self,
        provider_kind: &str,
        resolved: &ResolvedProviderRuntime,
    ) -> Result<(Url, PreparedProviderTarget)> {
        let embeddings_path = resolved
            .runtime
            .embeddings_path
            .as_deref()
            .ok_or_else(|| anyhow!("provider {provider_kind} does not support embeddings"))?;
        let endpoint_url =
            provider_endpoint_url(provider_kind, &resolved.base_url, embeddings_path)?;
        let target =
            self.transport.prepare(&endpoint_url, resolved.allow_private_network).await.map_err(
                |error| {
                    ProviderCallError::target_preparation(
                        &format!("provider target policy failed: provider={provider_kind}"),
                        &error,
                    )
                },
            )?;
        Ok((endpoint_url, target))
    }

    /// Resolves the chat endpoint and applies the outbound network policy
    /// without taking an attempt permit. Each HTTP attempt takes its lane
    /// permit immediately before dispatch so retry backoff never occupies the
    /// other lane's capacity.
    pub(super) async fn open_chat_endpoint(
        &self,
        provider_kind: &str,
        resolved: &ResolvedProviderRuntime,
    ) -> Result<(Url, PreparedProviderTarget)> {
        let endpoint_url =
            provider_endpoint_url(provider_kind, &resolved.base_url, &resolved.runtime.chat_path)?;
        let target =
            self.transport.prepare(&endpoint_url, resolved.allow_private_network).await.map_err(
                |error| {
                    ProviderCallError::target_preparation(
                        &format!("provider target policy failed: provider={provider_kind}"),
                        &error,
                    )
                },
            )?;
        Ok((endpoint_url, target))
    }

    pub(super) async fn begin_provider_attempt(
        &self,
        provider_kind: &str,
        base_url: &str,
    ) -> Result<ProviderBudgetGuard, ProviderCallError> {
        let identity = ProviderIdentity::new(provider_kind, base_url);
        acquire_provider_budget(&self.provider_budget, &identity).await
    }

    pub(super) fn prepared_request(
        target: &PreparedProviderTarget,
        method: Method,
        endpoint: &Url,
        provider_kind: &str,
    ) -> Result<reqwest::RequestBuilder, ProviderCallError> {
        target.request(method, endpoint).map_err(|error| {
            ProviderCallError::protocol(format!(
                "provider request policy failed: provider={provider_kind}: {error}"
            ))
        })
    }

    pub(super) fn prepared_provider_request(
        target: &PreparedProviderTarget,
        endpoint: &Url,
        provider_kind: &str,
        accept: &str,
        auth_scheme: ProviderAuthScheme,
        api_key: Option<&str>,
    ) -> Result<reqwest::RequestBuilder, ProviderCallError> {
        let request_builder =
            Self::prepared_request(target, Method::POST, endpoint, provider_kind)?
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, accept);
        let request_builder = apply_provider_auth(request_builder, auth_scheme, api_key);
        Ok(request_builder)
    }

    /// Dispatches a gateway provider request. The gateway builds only POST
    /// requests ([`Self::prepared_provider_request`]), so it labels the shared
    /// provider span with that method.
    pub(super) async fn send_provider_request(
        request: reqwest::RequestBuilder,
        endpoint: &Url,
        provider_kind: &str,
        operation: &str,
    ) -> Result<reqwest::Response, reqwest::Error> {
        send_provider_http_request(request, endpoint, &Method::POST, provider_kind, operation).await
    }
}
