//! Strictly-typed HTTP client for the TypeSafe SystemOne decision endpoint.
//!
//! The client reuses the shared provider transport, retry classifier, redaction
//! and budget registry. It owns no HTTP client of its own and never treats a
//! provider kind or model name as anything but caller-supplied data.

use std::{sync::Arc, time::Duration};

use reqwest::{Method, Url, header::CONTENT_TYPE};
use zeroize::Zeroizing;

use crate::{
    integrations::{
        provider_budget::{ProviderBudgetGuard, ProviderBudgetRegistry, ProviderIdentity},
        retry::{
            ProviderCallError, RetryPolicy, acquire_provider_budget,
            provider_status_error_from_read, with_retry,
        },
    },
    shared::{
        outbound_http::read_response_bytes_with_limit,
        provider_http::{
            PROVIDER_ERROR_BODY_MAX_BYTES, PROVIDER_SUCCESS_BODY_MAX_BYTES, ProviderHttpTransport,
            ProviderHttpTransportConfig, send_provider_http_request,
        },
    },
};

use super::{
    protocol::{SystemOneDecisionRequest, SystemOneDecisionResponse},
    validation::{
        SystemOneConfigError, SystemOneError, SystemOneResponseError, validate_request,
        validate_response,
    },
};

/// Path of the SystemOne decision endpoint, appended to the configured base
/// URL's own path prefix.
pub(super) const SYSTEM_ONE_DECISIONS_PATH: &str = "/v1/systemone";

/// Diagnostics-only label for the shared status error and the request span when
/// no budget identity exists. It is never used for catalog lookup, provider
/// profile routing, or budget identity; a real identity always takes
/// precedence.
const SYSTEM_ONE_DIAGNOSTIC_PROVIDER_KIND: &str = "typesafe";

/// Budget identity and registry a caller attaches to the client. The identity
/// is caller-supplied so no provider kind is ever guessed here.
pub struct SystemOneBudget {
    pub registry: Arc<ProviderBudgetRegistry>,
    pub identity: ProviderIdentity,
}

/// Construction inputs for [`SystemOneClient`].
pub struct SystemOneClientConfig {
    /// Base URL of the TypeSafe API; any path prefix is preserved.
    pub base_url: Url,
    /// Provider credential. Kept in a `Zeroizing` buffer so the long-lived
    /// bearer token is wiped when the client is dropped, matching the crate's
    /// other secret-bearing fields.
    pub api_key: Zeroizing<String>,
    pub request_timeout: Duration,
    pub allow_private_network: bool,
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// Hard upper bound on one `evaluate` call, measured from before target
    /// preparation — which resolves and pins DNS — through the complete retry
    /// sequence. The client enforces it locally with a wall-clock timeout
    /// around both preparation and the retry group, so a pending DNS lookup or
    /// an advertised `Retry-After` larger than the remaining budget ends the
    /// call with a typed timeout error instead of hanging or sleeping past it.
    /// The shared `max_attempts` cap is unchanged.
    pub execution_deadline: Duration,
    pub budget: Option<SystemOneBudget>,
}

/// Caller-supplied transport policy for a SystemOne decision client: the HTTP
/// request timeout and the bounded retry envelope. The same type serves every
/// decision caller, so one constructor can own client assembly.
#[derive(Debug, Clone, Copy)]
pub struct SystemOneTransportParams {
    pub request_timeout: Duration,
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

/// Why a SystemOne decision client could not be built from a resolved binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOneClientBuildError {
    /// The binding declares no non-blank base URL.
    MissingBaseUrl,
    /// The binding's base URL is not a parsable absolute URL.
    InvalidBaseUrl,
    /// The binding declares no non-blank API key.
    MissingApiKey,
    /// The shared provider transport could not be built from the config.
    Config,
}

/// A reusable SystemOne decision client.
pub struct SystemOneClient {
    endpoint: Url,
    transport: ProviderHttpTransport,
    retry_policy: RetryPolicy<ProviderCallError>,
    execution_deadline: Duration,
    api_key: Zeroizing<String>,
    allow_private_network: bool,
    budget: Option<SystemOneBudget>,
}

impl SystemOneClient {
    /// Builds a client for one SystemOne endpoint.
    ///
    /// # Errors
    /// Fails closed when the configured API key is empty or whitespace-only, and
    /// when the shared provider transport cannot be built. The base URL's
    /// scheme, credentials, query and fragment are enforced by the shared
    /// provider transport (`ProviderHttpTransport::prepare`) before the first
    /// request, so URL policy is not re-implemented here.
    pub fn try_new(config: SystemOneClientConfig) -> Result<Self, SystemOneError> {
        // Sanitize the credential before it is stored: surrounding whitespace
        // would send a wrong bearer token, and an empty key would send a
        // credential-less request. The error carries the variant only, never the
        // rejected value.
        let api_key = config.api_key.trim();
        if api_key.is_empty() {
            return Err(SystemOneError::Config(SystemOneConfigError::EmptyApiKey));
        }
        let api_key = Zeroizing::new(api_key.to_string());
        let endpoint = system_one_endpoint_url(&config.base_url);
        let transport = ProviderHttpTransport::try_new(ProviderHttpTransportConfig::llm(
            config.request_timeout,
        ))
        .map_err(|error| SystemOneError::Provider {
            source: ProviderCallError::protocol(format!(
                "failed to build provider HTTP transport: {error}"
            )),
        })?;
        let retry_policy = RetryPolicy::idempotent_provider_call(
            config.max_attempts,
            config.initial_backoff,
            config.max_backoff,
        );
        Ok(Self {
            endpoint,
            transport,
            retry_policy,
            execution_deadline: config.execution_deadline,
            api_key,
            allow_private_network: config.allow_private_network,
            budget: config.budget,
        })
    }

    /// Test-only seam: replaces the client's transport with one built from
    /// injected dependencies, so a test can exercise target-preparation failure
    /// without real DNS or a live socket. Never compiled into production builds.
    #[cfg(test)]
    pub(super) fn with_transport(mut self, transport: ProviderHttpTransport) -> Self {
        self.transport = transport;
        self
    }

    /// Sends one decision request and returns its validated response.
    ///
    /// # Errors
    /// Returns a request validation, provider transport, HTTP status, decode,
    /// or response validation error. It never swallows a failure.
    pub async fn evaluate(
        &self,
        request: SystemOneDecisionRequest,
    ) -> Result<SystemOneDecisionResponse, SystemOneError> {
        let request = &request;
        validate_request(request)?;
        let body = serde_json::to_vec(request).map_err(|error| SystemOneError::Provider {
            source: ProviderCallError::json("systemone request serialization", error),
        })?;
        let diagnostic_label = self.diagnostic_provider_kind();
        // The success body is read inside the attempt so a transient body-read
        // failure follows the shared provider retry policy, exactly as the
        // buffered LLM chat path reads its body inside the retryable attempt.
        // Decode and validation stay outside the retry: malformed JSON and an
        // invalid decision are terminal and must never be retried.
        //
        // One local wall-clock timeout covers both target preparation — which
        // resolves and pins DNS — and the complete retry group, so the
        // network-bound portion of the call is bounded from before resolution
        // through the last attempt, not only from the first attempt.
        // The shared retry loop keeps its own `max_attempts` cap; the timeout
        // only ends a call still resolving, sleeping or reading past its
        // deadline, and never turns a bounded retry into an unbounded loop. On
        // expiry the in-flight future is dropped, which releases any provider
        // budget permit it held before the typed timeout error is returned. The
        // client performs no database mutation, so no non-idempotent operation
        // can be cancelled halfway.
        let (bytes, guard) = match tokio::time::timeout(self.execution_deadline, async {
            let target =
                self.transport.prepare(&self.endpoint, self.allow_private_network).await.map_err(
                    |error| {
                        ProviderCallError::target_preparation(SYSTEM_ONE_TARGET_CONTEXT, &error)
                    },
                )?;
            with_retry(|| self.attempt(&target, &body, diagnostic_label), self.retry_policy).await
        })
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                return Err(SystemOneError::Provider {
                    source: ProviderCallError::timeout(
                        "systemone decision exceeded its execution deadline",
                    ),
                });
            }
        };

        let decoded: SystemOneDecisionResponse =
            serde_json::from_slice(&bytes).map_err(|error| SystemOneError::Response {
                source: SystemOneResponseError::from(error),
                usage: None,
            })?;
        if let Err(source) = validate_response(request, &decoded) {
            return Err(SystemOneError::Response { source, usage: Some(decoded.usage) });
        }
        drop(guard);
        Ok(decoded)
    }

    async fn attempt(
        &self,
        target: &crate::shared::provider_http::PreparedProviderTarget,
        body: &[u8],
        diagnostic_label: &str,
    ) -> Result<(Vec<u8>, ProviderBudgetGuard), ProviderCallError> {
        let guard = self.begin_attempt().await?;
        let request = target
            .request(Method::POST, &self.endpoint)
            .map_err(|error| {
                ProviderCallError::protocol(format!(
                    "systemone request policy failed: provider={diagnostic_label}: {error}"
                ))
            })?
            .header(CONTENT_TYPE, "application/json")
            .bearer_auth(self.api_key.as_str())
            .body(body.to_vec());
        let response = self.send(request, diagnostic_label).await?;
        if !response.status().is_success() {
            let status = response.status();
            let headers = response.headers().clone();
            return Err(provider_status_error_from_read(
                diagnostic_label,
                status,
                &headers,
                read_response_bytes_with_limit(response, PROVIDER_ERROR_BODY_MAX_BYTES).await.ok(),
            ));
        }
        // A truncated or reset success body is a transport policy failure that
        // the shared classifier retries; oversize bodies abort. The read must
        // happen while the attempt's budget guard is still held, so the permit
        // covers the whole request-and-body unit and is released before backoff.
        let bytes = read_response_bytes_with_limit(response, PROVIDER_SUCCESS_BODY_MAX_BYTES)
            .await
            .map_err(|error| {
                ProviderCallError::response_policy("systemone response body", error)
            })?;
        Ok((bytes, guard))
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        diagnostic_label: &str,
    ) -> Result<reqwest::Response, ProviderCallError> {
        send_provider_http_request(
            request,
            &self.endpoint,
            &Method::POST,
            diagnostic_label,
            "systemone_decision",
        )
        .await
        .map_err(|error| ProviderCallError::transport("systemone provider request", error))
    }

    async fn begin_attempt(&self) -> Result<ProviderBudgetGuard, ProviderCallError> {
        let Some(budget) = &self.budget else {
            return Ok(ProviderBudgetGuard::Unlimited);
        };
        acquire_provider_budget(&budget.registry, &budget.identity).await
    }

    fn diagnostic_provider_kind(&self) -> &str {
        self.budget.as_ref().map_or(SYSTEM_ONE_DIAGNOSTIC_PROVIDER_KIND, |budget| {
            budget.identity.provider_kind.as_str()
        })
    }
}

/// Diagnostic context for a failed target preparation. It is a caller-owned
/// label, never a credential or a resolved address.
///
/// The typed mapping itself is the shared [`ProviderCallError::target_preparation`].
/// [`SystemOneClient::evaluate`] nests that error inside
/// [`SystemOneError::Provider`], and `ProviderFailureClassificationService`
/// downcasts only the top-level `anyhow` error and its context layers, never an
/// arbitrary `source()` chain: IRG-185/IRG-187 consumers must extract the
/// nested `ProviderCallError` and forward it for the classification to apply.
const SYSTEM_ONE_TARGET_CONTEXT: &str = "systemone provider target";

/// Joins the canonical decision path onto a base URL while preserving any base
/// path prefix.
///
/// URL policy (scheme, credentials, query, fragment) is enforced in one place by
/// the shared provider transport's validated target, which runs before every
/// request, so this is a pure path join rather than a second policy layer.
fn system_one_endpoint_url(base_url: &Url) -> Url {
    let mut endpoint = base_url.clone();
    let base_path = endpoint.path().trim_end_matches('/');
    endpoint.set_path(&format!("{base_path}{SYSTEM_ONE_DECISIONS_PATH}"));
    endpoint
}
