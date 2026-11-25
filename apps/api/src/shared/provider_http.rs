use std::{
    collections::VecDeque,
    fmt,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use reqwest::{
    Client, Method, RequestBuilder, Url,
    header::{HeaderName, HeaderValue},
};
use thiserror::Error;
use tracing::Instrument as _;
use uuid::Uuid;

use super::outbound_http::{PublicHttpUrlError, is_non_public_ip};
use super::provider_send;

pub const PROVIDER_SUCCESS_BODY_MAX_BYTES: u64 = 32 * 1024 * 1024;
pub const PROVIDER_ERROR_BODY_MAX_BYTES: u64 = 64 * 1024;
pub const PROVIDER_MODEL_LIST_BODY_MAX_BYTES: u64 = 8 * 1024 * 1024;
pub const PROVIDER_STREAM_TOTAL_MAX_BYTES: usize = 32 * 1024 * 1024;
pub const PROVIDER_STREAM_FRAME_MAX_BYTES: usize = 1024 * 1024;

const PROVIDER_HTTP_CACHE_CAPACITY: usize = 16;
const PROVIDER_HTTP_CACHE_TTL: Duration = Duration::from_mins(1);
const PROVIDER_DNS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderHttpTransportConfig {
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub pool_idle_timeout: Duration,
    pub dns_timeout: Duration,
    pub cache_ttl: Duration,
    pub cache_capacity: usize,
}

impl ProviderHttpTransportConfig {
    #[must_use]
    pub const fn llm(request_timeout: Duration) -> Self {
        Self {
            request_timeout,
            connect_timeout: Duration::from_secs(10),
            pool_idle_timeout: Duration::from_secs(30),
            dns_timeout: PROVIDER_DNS_TIMEOUT,
            cache_ttl: PROVIDER_HTTP_CACHE_TTL,
            cache_capacity: PROVIDER_HTTP_CACHE_CAPACITY,
        }
    }

    #[must_use]
    pub const fn provider_validation() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(10),
            pool_idle_timeout: Duration::from_secs(30),
            dns_timeout: PROVIDER_DNS_TIMEOUT,
            cache_ttl: PROVIDER_HTTP_CACHE_TTL,
            cache_capacity: PROVIDER_HTTP_CACHE_CAPACITY,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderHttpError {
    #[error("provider HTTP transport requires non-zero cache capacity and TTL")]
    InvalidCacheConfiguration,
    #[error("provider URL must use http or https")]
    ForbiddenScheme,
    #[error("provider URL must not contain credentials")]
    CredentialsForbidden,
    #[error("provider URL must not contain query or fragment components")]
    QueryOrFragmentForbidden,
    #[error("provider URL is missing a host")]
    MissingHost,
    #[error("provider URL is missing a port")]
    MissingPort,
    #[error("provider host resolution failed")]
    ResolveFailed,
    #[error("provider host resolution timed out")]
    ResolveTimedOut,
    #[error("provider host resolved to no addresses")]
    NoAddresses,
    #[error("provider host resolved to a non-public address ({0})")]
    NonPublicAddress(IpAddr),
    #[error("provider HTTP client construction failed")]
    ClientBuildFailed,
    #[error("prepared provider target cannot be used for another origin")]
    OriginMismatch,
}

impl ProviderHttpError {
    /// The typed public resolution failure this is, if any. DNS resolution
    /// failures are transient upstream conditions; every other variant is a URL,
    /// address or origin policy rejection that no retry can fix.
    #[must_use]
    pub fn resolution_failure(&self) -> Option<PublicHttpUrlError> {
        match self {
            Self::ResolveTimedOut => Some(PublicHttpUrlError::ResolveTimedOut),
            Self::ResolveFailed => Some(PublicHttpUrlError::ResolveFailed),
            Self::NoAddresses => Some(PublicHttpUrlError::NoAddresses),
            _ => None,
        }
    }
}

#[async_trait]
trait ProviderHostResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, ProviderHttpError>;
}

struct SystemProviderHostResolver {
    timeout: Duration,
}

#[async_trait]
impl ProviderHostResolver for SystemProviderHostResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, ProviderHttpError> {
        let lookup = tokio::time::timeout(self.timeout, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| ProviderHttpError::ResolveTimedOut)?
            .map_err(|_| ProviderHttpError::ResolveFailed)?;
        Ok(lookup.collect())
    }
}

/// Test-only resolver that records its call and then never completes, so a
/// bounded caller can prove its deadline cancels target preparation without
/// real DNS. It asserts the host it was asked for, tying the pending
/// resolution to one named host.
#[cfg(test)]
struct PendingProviderHostResolver {
    expected_host: String,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
#[async_trait]
impl ProviderHostResolver for PendingProviderHostResolver {
    async fn resolve(&self, host: &str, _port: u16) -> Result<Vec<SocketAddr>, ProviderHttpError> {
        assert_eq!(host, self.expected_host, "pending resolver is bound to one host");
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::future::pending::<Result<Vec<SocketAddr>, ProviderHttpError>>().await
    }
}

/// Test-only resolver that fails every lookup with one fixed error, so a caller
/// can prove how a DNS failure surfaces without touching real DNS.
#[cfg(test)]
struct FailingProviderHostResolver {
    error: ProviderHttpError,
}

#[cfg(test)]
#[async_trait]
impl ProviderHostResolver for FailingProviderHostResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, ProviderHttpError> {
        Err(self.error.clone())
    }
}

trait ProviderHttpClientFactory: Send + Sync {
    fn build(
        &self,
        config: ProviderHttpTransportConfig,
        dns_pin: Option<(&str, &[SocketAddr])>,
    ) -> Result<Client, ProviderHttpError>;
}

struct SystemProviderHttpClientFactory;

impl ProviderHttpClientFactory for SystemProviderHttpClientFactory {
    fn build(
        &self,
        config: ProviderHttpTransportConfig,
        dns_pin: Option<(&str, &[SocketAddr])>,
    ) -> Result<Client, ProviderHttpError> {
        let mut builder = Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .pool_idle_timeout(config.pool_idle_timeout)
            .tcp_keepalive(Duration::from_secs(15))
            .http2_keep_alive_interval(Duration::from_secs(15))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .http2_keep_alive_while_idle(true)
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none());
        if let Some((host, addresses)) = dns_pin {
            builder = builder.resolve_to_addrs(host, addresses);
        }
        builder.build().map_err(|_| ProviderHttpError::ClientBuildFailed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderHttpCacheKey {
    origin: String,
    allow_private_network: bool,
    request_timeout: Duration,
    connect_timeout: Duration,
    pool_idle_timeout: Duration,
}

struct CachedProviderClient {
    key: ProviderHttpCacheKey,
    client: Client,
    _addresses: Vec<SocketAddr>,
    expires_at: Instant,
}

pub struct ProviderHttpTransport {
    config: ProviderHttpTransportConfig,
    resolver: Arc<dyn ProviderHostResolver>,
    client_factory: Arc<dyn ProviderHttpClientFactory>,
    literal_client: Client,
    cache: tokio::sync::Mutex<VecDeque<CachedProviderClient>>,
}

impl fmt::Debug for ProviderHttpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderHttpTransport")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ProviderHttpTransport {
    /// Builds a bounded, no-redirect provider transport.
    ///
    /// # Errors
    /// Fails closed when the HTTP client cannot be built or the bounded cache
    /// configuration is invalid.
    pub fn try_new(config: ProviderHttpTransportConfig) -> Result<Self, ProviderHttpError> {
        let resolver = Arc::new(SystemProviderHostResolver { timeout: config.dns_timeout });
        Self::try_new_from_dependencies(config, resolver, Arc::new(SystemProviderHttpClientFactory))
    }

    #[cfg(test)]
    fn try_new_with_dependencies(
        config: ProviderHttpTransportConfig,
        resolver: Arc<dyn ProviderHostResolver>,
        client_factory: Arc<dyn ProviderHttpClientFactory>,
    ) -> Result<Self, ProviderHttpError> {
        Self::try_new_from_dependencies(config, resolver, client_factory)
    }

    /// Test-only transport built on a resolver that never completes for
    /// `expected_host`, so a bounded caller can prove its deadline cancels
    /// target preparation before any request is built. Returns the transport
    /// and the resolver call counter so a test can assert the DNS path ran
    /// without touching real DNS.
    #[cfg(test)]
    pub(crate) fn try_new_with_pending_resolver(
        config: ProviderHttpTransportConfig,
        expected_host: &str,
    ) -> Result<(Self, Arc<std::sync::atomic::AtomicUsize>), ProviderHttpError> {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resolver = Arc::new(PendingProviderHostResolver {
            expected_host: expected_host.to_owned(),
            calls: Arc::clone(&calls),
        });
        let transport = Self::try_new_from_dependencies(
            config,
            resolver,
            Arc::new(SystemProviderHttpClientFactory),
        )?;
        Ok((transport, calls))
    }

    /// Test-only transport whose every host resolution fails with `error`.
    #[cfg(test)]
    pub(crate) fn try_new_with_failing_resolver(
        config: ProviderHttpTransportConfig,
        error: ProviderHttpError,
    ) -> Result<Self, ProviderHttpError> {
        Self::try_new_from_dependencies(
            config,
            Arc::new(FailingProviderHostResolver { error }),
            Arc::new(SystemProviderHttpClientFactory),
        )
    }

    fn try_new_from_dependencies(
        config: ProviderHttpTransportConfig,
        resolver: Arc<dyn ProviderHostResolver>,
        client_factory: Arc<dyn ProviderHttpClientFactory>,
    ) -> Result<Self, ProviderHttpError> {
        if config.cache_capacity == 0 || config.cache_ttl.is_zero() {
            return Err(ProviderHttpError::InvalidCacheConfiguration);
        }
        let literal_client = client_factory.build(config, None)?;
        Ok(Self {
            config,
            resolver,
            client_factory,
            literal_client,
            cache: tokio::sync::Mutex::new(VecDeque::with_capacity(config.cache_capacity)),
        })
    }

    /// Resolves and DNS-pins a provider origin. A fresh pin is cached for at
    /// most 60 seconds and the cache is bounded to 16 origins by default.
    ///
    /// # Errors
    /// Rejects malformed URLs, DNS failures, non-public targets without an
    /// explicit private-network opt-in, and client build failures.
    pub async fn prepare(
        &self,
        endpoint: &Url,
        allow_private_network: bool,
    ) -> Result<PreparedProviderTarget, ProviderHttpError> {
        let target = ValidatedProviderTarget::from_url(endpoint)?;
        let key = ProviderHttpCacheKey {
            origin: target.origin.clone(),
            allow_private_network,
            request_timeout: self.config.request_timeout,
            connect_timeout: self.config.connect_timeout,
            pool_idle_timeout: self.config.pool_idle_timeout,
        };
        if let Some(client) = self.cached_client(&key).await {
            return Ok(PreparedProviderTarget { client, origin: target.origin });
        }

        let mut addresses = if let Ok(ip) = target.host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, target.port)]
        } else {
            self.resolver.resolve(&target.host, target.port).await?
        };
        addresses.sort_unstable();
        addresses.dedup();
        if addresses.is_empty() {
            return Err(ProviderHttpError::NoAddresses);
        }
        if !allow_private_network
            && let Some(address) = addresses.iter().find(|address| is_non_public_ip(address.ip()))
        {
            return Err(ProviderHttpError::NonPublicAddress(address.ip()));
        }

        let client = if target.host.parse::<IpAddr>().is_ok() {
            self.literal_client.clone()
        } else {
            self.client_factory.build(self.config, Some((&target.host, &addresses)))?
        };
        let client = self.insert_or_use_cached(key, client, addresses).await;
        Ok(PreparedProviderTarget { client, origin: target.origin })
    }

    async fn cached_client(&self, key: &ProviderHttpCacheKey) -> Option<Client> {
        let now = Instant::now();
        let mut cache = self.cache.lock().await;
        cache.retain(|entry| entry.expires_at > now);
        let position = cache.iter().position(|entry| &entry.key == key)?;
        let entry = cache.remove(position)?;
        let client = entry.client.clone();
        cache.push_back(entry);
        drop(cache);
        Some(client)
    }

    async fn insert_or_use_cached(
        &self,
        key: ProviderHttpCacheKey,
        client: Client,
        addresses: Vec<SocketAddr>,
    ) -> Client {
        let now = Instant::now();
        let mut cache = self.cache.lock().await;
        cache.retain(|entry| entry.expires_at > now);
        if let Some(position) = cache.iter().position(|entry| entry.key == key)
            && let Some(entry) = cache.remove(position)
        {
            let cached = entry.client.clone();
            cache.push_back(entry);
            return cached;
        }
        while cache.len() >= self.config.cache_capacity {
            cache.pop_front();
        }
        cache.push_back(CachedProviderClient {
            key,
            client: client.clone(),
            _addresses: addresses,
            expires_at: now + self.config.cache_ttl,
        });
        client
    }
}

struct ValidatedProviderTarget {
    origin: String,
    host: String,
    port: u16,
}

impl ValidatedProviderTarget {
    fn from_url(url: &Url) -> Result<Self, ProviderHttpError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ProviderHttpError::ForbiddenScheme);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ProviderHttpError::CredentialsForbidden);
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(ProviderHttpError::QueryOrFragmentForbidden);
        }
        let host = url.host_str().ok_or(ProviderHttpError::MissingHost)?.to_string();
        let port = url.port_or_known_default().ok_or(ProviderHttpError::MissingPort)?;
        Ok(Self { origin: url.origin().ascii_serialization(), host, port })
    }
}

#[derive(Clone, Debug)]
pub struct PreparedProviderTarget {
    client: Client,
    origin: String,
}

impl PreparedProviderTarget {
    /// Creates a request only when `endpoint` has the exact origin that was
    /// resolved and pinned by [`ProviderHttpTransport::prepare`].
    ///
    /// # Errors
    /// Rejects malformed or cross-origin endpoint URLs before callers can add
    /// provider credentials to the request.
    pub fn request(
        &self,
        method: Method,
        endpoint: &Url,
    ) -> Result<RequestBuilder, ProviderHttpError> {
        let target = ValidatedProviderTarget::from_url(endpoint)?;
        if target.origin != self.origin {
            return Err(ProviderHttpError::OriginMismatch);
        }
        Ok(self.client.request(method, endpoint.clone()))
    }
}

/// Upper bound on an inspectable JSON provider request body. Larger bodies are
/// treated as uninspectable and refused instead of being buffered without limit.
pub const PROVIDER_REQUEST_BODY_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Who authorized one physical provider send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderDispatchAuthority {
    /// A resolved runtime binding. The only authority for ordinary traffic.
    RuntimeBinding,
    /// The authorized account-creation credential validation, which runs before
    /// any binding or account row exists. Refused under a required incident scope.
    AccountValidation,
    /// Ordinary-mode vision. The incident envelope is text-only, so this is
    /// never valid under a configured incident scope; it exists so vision keeps
    /// working in a runtime with no incident scope and still uses the one path.
    OrdinaryVision,
}

/// Which provider surface the attested route belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSurface {
    Chat,
    Embeddings,
    Decision,
}

/// The natively authenticated caller of one paid provider attempt.
///
/// Every value is derived from an authentication the request already proved:
/// the server-side MCP session digest, the browser session row, or the mutation
/// origin the admitting request recorded. Nothing here is ever read from a
/// policy, a configuration value, a CLI argument, a request body, a request
/// label or an error message — those can only ever leave a dispatch
/// [`Unattributed`](Self::Unattributed), which a fenced scope refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchCaller {
    McpSession([u8; 32]),
    IamSession(Uuid),
    MutationOrigin {
        mutation_id: Uuid,
        principal_id: Uuid,
    },
    /// No native caller bound this attempt: direct API tokens, maintenance
    /// producers and connector-origin work with no recorded principal.
    Unattributed,
}

tokio::task_local! {
    static DISPATCH_CALLER: DispatchCaller;
}

impl DispatchCaller {
    /// The caller a queued job is dispatched for, derived from the record that
    /// admitted it: the mutation id plus the principal that mutation names.
    /// A missing or nil principal is [`Self::Unattributed`], never a worker
    /// identity, so a grant without a matching entry refuses the attempt.
    #[must_use]
    pub fn mutation_origin(mutation_id: Uuid, requested_by_principal_id: Option<Uuid>) -> Self {
        match requested_by_principal_id {
            Some(principal_id) if !principal_id.is_nil() && !mutation_id.is_nil() => {
                Self::MutationOrigin { mutation_id, principal_id }
            }
            _ => Self::Unattributed,
        }
    }
}

/// Run `future` under the caller one authenticated request proved.
///
/// Installed only by the boundary that validated the caller: the HTTP auth
/// middleware, the MCP session transport, and the ingest job that reads its own
/// recorded mutation origin. The scope follows `tokio::join!` /
/// `try_join!` parallelism inside the same task and ends when the future ends.
pub(crate) async fn with_dispatch_caller<F>(caller: DispatchCaller, future: F) -> F::Output
where
    F: std::future::Future,
{
    DISPATCH_CALLER.scope(caller, future).await
}

/// The caller of the current request scope, or `Unattributed` outside one.
#[must_use]
pub(crate) fn current_dispatch_caller() -> DispatchCaller {
    DISPATCH_CALLER.try_with(|caller| *caller).unwrap_or(DispatchCaller::Unattributed)
}

/// Sealed, non-serializable attestation of the resolved route and credential.
///
/// Everything here is derived from the authoritative runtime binding (or the
/// authorized account-validation operation) before a request is built. It is
/// deliberately not `Deserialize`, its fields are private, and it is never
/// included in a provider body, a log line or an embedding profile key.
#[derive(Clone)]
pub struct ProviderDispatchContext {
    authority: ProviderDispatchAuthority,
    surface: ProviderSurface,
    provider_kind: String,
    provider_catalog_id: Uuid,
    account_id: Option<Uuid>,
    model_name: String,
    base_url: String,
    auth: Option<AttestedCredential>,
    caller: DispatchCaller,
}

/// The exact credential header a resolved binding attaches, reduced to its
/// name and a digest. The raw secret is never stored twice.
#[derive(Debug, Clone)]
struct AttestedCredential {
    header: HeaderName,
    value_digest: [u8; 32],
}

impl std::fmt::Debug for ProviderDispatchContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderDispatchContext")
            .field("authority", &self.authority)
            .field("surface", &self.surface)
            .field("provider_kind", &self.provider_kind)
            .field("model_name", &self.model_name)
            .finish_non_exhaustive()
    }
}

/// The authoritative, credential-free part of a binding's dispatch identity.
///
/// The runtime resolver creates it from a [`crate::services::ai_catalog_service::
/// ResolvedRuntimeBinding`]; provider callers receive it and cannot fabricate it
/// from request labels because the constructor is crate-visible and the fields
/// are private.
#[derive(Debug, Clone)]
pub struct DispatchAuthority {
    kind: ProviderDispatchAuthority,
    provider_kind: String,
    provider_catalog_id: Uuid,
    account_id: Option<Uuid>,
    model_name: String,
    base_url: String,
    credential_digest: Option<[u8; 32]>,
}

/// The route and credential identity a [`DispatchAuthority`] is issued for.
///
/// Grouped so the three authority constructors share one digest rule without a
/// positional argument list that grows with every identity field.
struct DispatchIdentity {
    provider_kind: String,
    provider_catalog_id: Uuid,
    account_id: Option<Uuid>,
    model_name: String,
    base_url: String,
    credential_digest: Option<[u8; 32]>,
}

impl DispatchIdentity {
    fn new(
        provider_kind: impl Into<String>,
        provider_catalog_id: Uuid,
        account_id: Option<Uuid>,
        model_name: impl Into<String>,
        base_url: impl Into<String>,
        credential: Option<&str>,
    ) -> Self {
        Self {
            provider_kind: provider_kind.into(),
            provider_catalog_id,
            account_id,
            model_name: model_name.into(),
            base_url: base_url.into(),
            credential_digest: credential
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(credential_digest),
        }
    }
}

impl DispatchAuthority {
    pub(crate) fn runtime_binding(
        provider_kind: impl Into<String>,
        provider_catalog_id: Uuid,
        account_id: Option<Uuid>,
        model_name: impl Into<String>,
        base_url: impl Into<String>,
        credential: Option<&str>,
    ) -> Self {
        Self::new(
            ProviderDispatchAuthority::RuntimeBinding,
            DispatchIdentity::new(
                provider_kind,
                provider_catalog_id,
                account_id,
                model_name,
                base_url,
                credential,
            ),
        )
    }

    /// The narrow, authorized account-creation validation authority. It exists
    /// only because that operation validates a proposed credential before the
    /// account or any binding exists; admission refuses it under a required scope.
    pub(crate) fn account_validation(
        provider_kind: impl Into<String>,
        provider_catalog_id: Uuid,
        account_id: Option<Uuid>,
        model_name: impl Into<String>,
        base_url: impl Into<String>,
        credential: Option<&str>,
    ) -> Self {
        Self::new(
            ProviderDispatchAuthority::AccountValidation,
            DispatchIdentity::new(
                provider_kind,
                provider_catalog_id,
                account_id,
                model_name,
                base_url,
                credential,
            ),
        )
    }

    /// Ordinary-mode vision authority. Never valid under a configured incident
    /// scope, because the incident envelope is text-only.
    pub(crate) fn ordinary_vision(
        provider_kind: impl Into<String>,
        model_name: impl Into<String>,
        base_url: impl Into<String>,
        credential: Option<&str>,
    ) -> Self {
        Self::new(
            ProviderDispatchAuthority::OrdinaryVision,
            DispatchIdentity::new(
                provider_kind,
                Uuid::nil(),
                None,
                model_name,
                base_url,
                credential,
            ),
        )
    }

    fn new(kind: ProviderDispatchAuthority, identity: DispatchIdentity) -> Self {
        Self {
            kind,
            provider_kind: identity.provider_kind,
            provider_catalog_id: identity.provider_catalog_id,
            account_id: identity.account_id,
            model_name: identity.model_name,
            base_url: identity.base_url,
            credential_digest: identity.credential_digest,
        }
    }

    #[must_use]
    pub fn kind(&self) -> ProviderDispatchAuthority {
        self.kind
    }

    #[must_use]
    pub fn provider_kind(&self) -> &str {
        &self.provider_kind
    }

    #[must_use]
    pub fn provider_catalog_id(&self) -> Uuid {
        self.provider_catalog_id
    }

    #[must_use]
    pub fn account_id(&self) -> Option<Uuid> {
        self.account_id
    }

    #[must_use]
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn credential_digest(&self) -> Option<[u8; 32]> {
        self.credential_digest
    }

    /// Bind this authority to one concrete surface and authentication header.
    #[must_use]
    pub fn attest(
        &self,
        surface: ProviderSurface,
        auth_header: Option<(HeaderName, String)>,
    ) -> ProviderDispatchContext {
        ProviderDispatchContext {
            authority: self.kind,
            surface,
            provider_kind: self.provider_kind.clone(),
            provider_catalog_id: self.provider_catalog_id,
            account_id: self.account_id,
            model_name: self.model_name.clone(),
            base_url: self.base_url.clone(),
            auth: auth_header.map(|(header, value)| AttestedCredential {
                header,
                value_digest: credential_digest(&value),
            }),
            // Read per attestation, never copied from a binding, policy or
            // request label: the authoritative value is the request scope's
            // validated caller.
            caller: current_dispatch_caller(),
        }
    }
}

fn credential_digest(value: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(value.as_bytes()).into()
}

/// Digest of a credential, comparable between the resolver and the boundary
/// without storing the secret twice.
#[must_use]
pub(crate) fn credential_fingerprint(value: &str) -> [u8; 32] {
    credential_digest(value)
}

impl ProviderDispatchContext {
    #[must_use]
    pub fn authority(&self) -> ProviderDispatchAuthority {
        self.authority
    }

    #[must_use]
    pub fn surface(&self) -> ProviderSurface {
        self.surface
    }

    #[must_use]
    pub fn provider_kind(&self) -> &str {
        &self.provider_kind
    }

    #[must_use]
    pub fn provider_catalog_id(&self) -> Uuid {
        self.provider_catalog_id
    }

    #[must_use]
    pub fn account_id(&self) -> Option<Uuid> {
        self.account_id
    }

    #[must_use]
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// The natively authenticated caller this attempt is dispatched for.
    #[must_use]
    pub fn caller(&self) -> DispatchCaller {
        self.caller
    }

    /// Whether `endpoint` stays under the resolved base URL this context was
    /// issued for. A caller cannot redirect an attested binding elsewhere.
    #[must_use]
    pub fn authorizes_route(&self, endpoint: &Url) -> bool {
        let Ok(base) = Url::parse(&self.base_url) else {
            return false;
        };
        endpoint.scheme() == base.scheme()
            && endpoint.host_str() == base.host_str()
            && endpoint.port_or_known_default() == base.port_or_known_default()
            && endpoint.path().starts_with(base.path())
    }

    /// Whether the credential actually attached to the built request is the one
    /// this context was issued for. Compares digests; no secret leaves memory.
    #[must_use]
    pub fn authorizes_credential(&self, request: &reqwest::Request) -> bool {
        match &self.auth {
            None => true,
            Some(auth) => request
                .headers()
                .get(&auth.header)
                .is_some_and(|value| credential_digest_header(value) == auth.value_digest),
        }
    }
}

fn credential_digest_header(value: &HeaderValue) -> [u8; 32] {
    credential_digest(value.to_str().unwrap_or_default())
}

/// Typed refusal of one physical provider attempt. No variant carries a secret
/// or a provider body.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderDispatchRefusal {
    #[error("provider dispatch is not authorized for this configuration")]
    Authority,
    #[error("provider dispatch route does not match the resolved binding")]
    RouteMismatch,
    #[error("provider dispatch credential does not match the resolved binding")]
    CredentialMismatch,
    #[error("provider dispatch is not authorized for this caller")]
    Caller,
    #[error("provider dispatch has no durable attribution for this send")]
    Attribution,
    #[error("provider dispatch already has a send in flight for this logical call")]
    Busy,
    #[error("provider request body is not an inspectable bounded JSON object")]
    UninspectableBody,
    #[error("provider request is outside the admitted request envelope")]
    Envelope,
    #[error("financial admission could not durably reserve this attempt")]
    Admission,
    #[error("financial allowance exhausted")]
    Exhausted,
}

/// One acknowledged durable reservation of a full attempt unit.
///
/// The permit is proof, not credit: dropping it never returns allowance. When
/// the admission owner created a durable send row for this attempt, the permit
/// also owns that row's transport lifetime: a permit dropped before the caller
/// settles it terminalizes the row without usage, so a canceled or dropped send
/// is visible as unresolved instead of disappearing.
pub struct ProviderDispatchPermit {
    scope_id: Option<Uuid>,
    send: Option<ProviderSendPermit>,
}

/// The durable row of one admitted physical send.
///
/// It is armed from the moment the row and the counter commit in one
/// transaction and stays armed until the reply is accepted. `complete` hands
/// the row to the caller's settlement; `fail` and a drop terminalize it without
/// usage, which the cost coverage reports as unresolved rather than free.
pub struct ProviderSendPermit {
    provider_call_id: Uuid,
    terminator: Option<ProviderSendTerminator>,
    armed: bool,
}

/// Terminalizes a send row from a context that has no application state, such
/// as a dropped transport future. It must never fabricate usage.
pub type ProviderSendTerminator = Arc<dyn Fn(Uuid, &'static str) + Send + Sync>;

impl ProviderSendPermit {
    #[must_use]
    pub fn new(provider_call_id: Uuid, terminator: Option<ProviderSendTerminator>) -> Self {
        Self { provider_call_id, terminator, armed: true }
    }

    #[must_use]
    pub fn provider_call_id(&self) -> Uuid {
        self.provider_call_id
    }

    fn terminalize(&mut self, call_state: &'static str) {
        if !self.armed {
            return;
        }
        self.armed = false;
        if let Some(terminator) = &self.terminator {
            terminator(self.provider_call_id, call_state);
        }
        provider_send::finish_current_send_row(self.provider_call_id);
    }

    /// The reply was accepted. The row stays open for the caller's settlement,
    /// and the scope may claim a new row if the caller asks for another send.
    fn accept_reply(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        provider_send::finish_current_send_row(self.provider_call_id);
    }
}

impl Drop for ProviderSendPermit {
    fn drop(&mut self) {
        // A dropped send had no accepted reply, so its row must not stay
        // `started` and must not claim usage. The reaper remains the last
        // resort, not the normal path.
        self.terminalize("canceled");
    }
}

impl std::fmt::Debug for ProviderDispatchPermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderDispatchPermit")
            .field("scope_id", &self.scope_id)
            .field("send_id", &self.send_id())
            .finish()
    }
}

impl std::fmt::Debug for ProviderSendPermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderSendPermit")
            .field("provider_call_id", &self.provider_call_id)
            .field("armed", &self.armed)
            .finish()
    }
}

impl ProviderDispatchPermit {
    #[must_use]
    pub fn unqualified() -> Self {
        Self { scope_id: None, send: None }
    }

    #[must_use]
    pub fn reserved(scope_id: Uuid) -> Self {
        Self { scope_id: Some(scope_id), send: None }
    }

    #[must_use]
    pub fn with_send(scope_id: Option<Uuid>, send: ProviderSendPermit) -> Self {
        Self { scope_id, send: Some(send) }
    }

    /// The incident scope whose attempt unit this permit reserved, or `None`
    /// when admission ran without a required scope.
    #[must_use]
    pub fn scope_id(&self) -> Option<Uuid> {
        self.scope_id
    }

    /// The durable row of this physical send, if the admission owner created
    /// one for this authority.
    #[must_use]
    pub fn send_id(&self) -> Option<Uuid> {
        self.send.as_ref().map(ProviderSendPermit::provider_call_id)
    }

    /// Accepts the reply: the row stays `started` for the caller's settlement,
    /// which is the only place that may record usage.
    pub fn complete_send(&mut self) {
        if let Some(send) = self.send.as_mut() {
            send.accept_reply();
        }
    }

    /// Rejects the reply before any retry may start. The row becomes `failed`
    /// without usage, so the next physical attempt owns a new row.
    pub fn fail_send(&mut self) {
        if let Some(send) = self.send.as_mut() {
            send.terminalize("failed");
        }
    }
}

/// The transport result of one admitted physical send, carrying the permit that
/// owns the matching durable row.
pub struct ProviderSendResponse {
    response: reqwest::Response,
    permit: ProviderDispatchPermit,
}

impl ProviderSendResponse {
    #[must_use]
    pub fn into_parts(self) -> (reqwest::Response, ProviderDispatchPermit) {
        (self.response, self.permit)
    }

    #[must_use]
    pub fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }
}

/// Test-only caller scope, so an integration test can drive the real transport
/// with the same authenticated caller a product request carries.
#[cfg(feature = "test-support")]
pub async fn with_test_dispatch_caller<F>(caller: DispatchCaller, future: F) -> F::Output
where
    F: std::future::Future,
{
    with_dispatch_caller(caller, future).await
}

/// Test-only runtime-binding authority builder for a synthetic provider fixture.
#[cfg(feature = "test-support")]
pub fn test_runtime_binding_authority(
    provider_kind: impl Into<String>,
    provider_catalog_id: Uuid,
    model_name: impl Into<String>,
    base_url: impl Into<String>,
) -> DispatchAuthority {
    DispatchAuthority::runtime_binding(
        provider_kind,
        provider_catalog_id,
        None,
        model_name,
        base_url,
        None,
    )
}

/// The single owner that may admit a built provider request.
///
/// Every physical send requires one, so a caller cannot construct a request and
/// dispatch it directly. Implementations reserve one full attempt unit before
/// the first byte leaves and never refund it.
#[async_trait]
pub trait ProviderDispatchAdmission: Send + Sync {
    async fn admit(
        &self,
        context: &ProviderDispatchContext,
        evidence: &DispatchRequestEvidence<'_>,
    ) -> Result<ProviderDispatchPermit, ProviderDispatchRefusal>;
}

/// What the admission owner may inspect about the built request.
#[derive(Clone, Copy)]
pub struct DispatchRequestEvidence<'a> {
    pub method: &'a Method,
    pub url: &'a Url,
    pub body: &'a serde_json::Value,
}

/// Why one provider attempt did not produce a response.
#[derive(Debug, Error)]
pub enum ProviderDispatchError {
    #[error(transparent)]
    Refused(#[from] ProviderDispatchRefusal),
    #[error("provider request could not be built")]
    Build,
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
}

impl ProviderDispatchError {
    /// The typed refusal, when this attempt never left the process.
    #[must_use]
    pub const fn refusal(&self) -> Option<&ProviderDispatchRefusal> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            Self::Build | Self::Transport(_) => None,
        }
    }
}

/// Extracts the admitted JSON body from the built request, or refuses.
fn inspect_request_body(
    request: &reqwest::Request,
) -> Result<serde_json::Value, ProviderDispatchRefusal> {
    let bytes = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .ok_or(ProviderDispatchRefusal::UninspectableBody)?;
    if bytes.is_empty() || bytes.len() > PROVIDER_REQUEST_BODY_MAX_BYTES {
        return Err(ProviderDispatchRefusal::UninspectableBody);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| ProviderDispatchRefusal::UninspectableBody)?;
    if !value.is_object() {
        return Err(ProviderDispatchRefusal::UninspectableBody);
    }
    Ok(value)
}

/// Sends a prepared provider request under the canonical provider HTTP span.
///
/// This is the shared dispatch path for the provider integration requests that
/// call it — the LLM gateway's chat, embedding and tool-use requests and the
/// SystemOne decision request — so their tracing attributes
/// (`provider.http.request`, `provider.operation`,
/// `http.response.status_code`, trace-context injection) cannot drift apart.
/// The caller supplies `method`, so a non-POST caller is labelled with its real
/// HTTP method instead of a hardcoded one. Callers keep their own error mapping;
/// this function returns the raw transport result.
///
/// It is not the only provider HTTP path: the AI-catalog credential-validation
/// model-list probe dispatches its own `GET` directly, because it maps failures
/// to credential-validation outcomes rather than provider call errors, so it
/// deliberately does not route here.
pub async fn send_provider_http_request(
    request: RequestBuilder,
    context: &ProviderDispatchContext,
    admission: &dyn ProviderDispatchAdmission,
    operation: &str,
) -> Result<ProviderSendResponse, ProviderDispatchError> {
    let (client, request) = crate::observability::inject_trace_context(request).build_split();
    let request = request.map_err(|_| ProviderDispatchError::Build)?;

    if request.method() != Method::POST || !context.authorizes_route(request.url()) {
        return Err(ProviderDispatchRefusal::RouteMismatch.into());
    }
    if !context.authorizes_credential(&request) {
        return Err(ProviderDispatchRefusal::CredentialMismatch.into());
    }
    let body = inspect_request_body(&request)?;
    let evidence =
        DispatchRequestEvidence { method: request.method(), url: request.url(), body: &body };
    // The admission owner creates the durable send row and debits the counter in
    // one transaction before the first byte leaves, and the permit owns that row
    // for the rest of the transport.
    let mut permit = admission.admit(context, &evidence).await?;

    let server_address = request.url().host_str().unwrap_or("unknown").to_owned();
    let method_label = request.method().as_str().to_owned();
    let provider_kind = context.provider_kind().to_owned();
    let provider_call_id = crate::observability::current_provider_call_id().unwrap_or_default();
    let span = tracing::info_span!(
        "provider.http.request",
        otel.kind = "client",
        "http.request.method" = method_label,
        "server.address" = server_address,
        "provider.kind" = provider_kind,
        "provider.operation" = operation,
        "provider.call.id" = provider_call_id,
        "http.response.status_code" = tracing::field::Empty,
    );
    let started = std::time::Instant::now();
    let response = async move { client.execute(request).await }.instrument(span.clone()).await;
    let outcome = if response.is_ok() { "ok" } else { "error" };
    crate::observability::metrics::record_provider_http_duration(
        context.provider_kind(),
        operation,
        outcome,
        started.elapsed().as_secs_f64(),
    );
    match response {
        Ok(response) => {
            span.record("http.response.status_code", response.status().as_u16());
            Ok(ProviderSendResponse { response, permit })
        }
        Err(error) => {
            // No reply reached the caller, so this physical attempt carries no
            // usage and must be terminal before any retry starts.
            permit.fail_send();
            Err(error.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use reqwest::{Client, Method, Url};

    use super::{
        ProviderHostResolver, ProviderHttpClientFactory, ProviderHttpError, ProviderHttpTransport,
        ProviderHttpTransportConfig, send_provider_http_request,
    };

    struct StaticResolver {
        calls: AtomicUsize,
        addresses: Vec<SocketAddr>,
    }

    #[async_trait]
    impl ProviderHostResolver for StaticResolver {
        async fn resolve(
            &self,
            _host: &str,
            _port: u16,
        ) -> Result<Vec<SocketAddr>, ProviderHttpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.addresses.clone())
        }
    }

    struct FailingClientFactory;

    impl ProviderHttpClientFactory for FailingClientFactory {
        fn build(
            &self,
            _config: ProviderHttpTransportConfig,
            _dns_pin: Option<(&str, &[SocketAddr])>,
        ) -> Result<Client, ProviderHttpError> {
            Err(ProviderHttpError::ClientBuildFailed)
        }
    }

    fn test_config() -> ProviderHttpTransportConfig {
        ProviderHttpTransportConfig {
            request_timeout: Duration::from_secs(2),
            connect_timeout: Duration::from_secs(1),
            pool_idle_timeout: Duration::from_secs(30),
            dns_timeout: Duration::from_secs(1),
            cache_ttl: Duration::from_secs(60),
            cache_capacity: 16,
        }
    }

    #[test]
    fn client_build_failure_is_typed_and_never_falls_back() {
        let resolver =
            Arc::new(StaticResolver { calls: AtomicUsize::new(0), addresses: Vec::new() });
        let error = ProviderHttpTransport::try_new_with_dependencies(
            test_config(),
            resolver,
            Arc::new(FailingClientFactory),
        )
        .expect_err("client construction must fail closed");

        assert_eq!(error, ProviderHttpError::ClientBuildFailed);
    }

    #[tokio::test]
    async fn public_looking_hostname_resolving_to_loopback_is_rejected() {
        let resolver = Arc::new(StaticResolver {
            calls: AtomicUsize::new(0),
            addresses: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443)],
        });
        let transport = ProviderHttpTransport::try_new_with_dependencies(
            test_config(),
            resolver,
            Arc::new(super::SystemProviderHttpClientFactory),
        )
        .expect("transport config should build");
        let endpoint = Url::parse("https://provider.example/v1/models").expect("test URL");

        let error = transport
            .prepare(&endpoint, false)
            .await
            .expect_err("public policy must reject a rebinding target");

        assert!(matches!(
            error,
            ProviderHttpError::NonPublicAddress(address) if address.is_loopback()
        ));
    }

    #[tokio::test]
    async fn private_opt_in_is_explicit_and_cached_without_re_resolving() {
        let resolver = Arc::new(StaticResolver {
            calls: AtomicUsize::new(0),
            addresses: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080)],
        });
        let transport = ProviderHttpTransport::try_new_with_dependencies(
            test_config(),
            resolver.clone(),
            Arc::new(super::SystemProviderHttpClientFactory),
        )
        .expect("transport config should build");
        let endpoint =
            Url::parse("http://local-provider.example:8080/v1/models").expect("test URL");

        let first = transport.prepare(&endpoint, true).await.expect("private opt-in should work");
        let second = transport.prepare(&endpoint, true).await.expect("cache hit should work");

        let _ = first.request(Method::GET, &endpoint).expect("same-origin request");
        let _ = second.request(Method::POST, &endpoint).expect("same-origin request");
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn prepared_target_rejects_cross_origin_request_before_auth_can_be_applied() {
        let resolver = Arc::new(StaticResolver {
            calls: AtomicUsize::new(0),
            addresses: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080)],
        });
        let transport = ProviderHttpTransport::try_new_with_dependencies(
            test_config(),
            resolver,
            Arc::new(super::SystemProviderHttpClientFactory),
        )
        .expect("transport config should build");
        let endpoint =
            Url::parse("http://local-provider.example:8080/v1/models").expect("test URL");
        let target = transport.prepare(&endpoint, true).await.expect("prepare target");
        let foreign = Url::parse("http://credential-sink.example:8080/steal").expect("foreign URL");

        let error = target
            .request(Method::GET, &foreign)
            .expect_err("cross-origin request must fail before request builder creation");

        assert_eq!(error, ProviderHttpError::OriginMismatch);
    }

    /// Write end that appends every formatted span event to a shared buffer, so
    /// a test can assert the dispatch span's method label.
    #[derive(Clone)]
    struct CapturingWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturingWriter {
        type Writer = CapturingWriter;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn provider_dispatch_span_records_the_callers_http_method() {
        use tracing_subscriber::layer::SubscriberExt as _;

        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(CapturingWriter(Arc::clone(&captured)))
                .with_target(false)
                .with_ansi(false)
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW),
        );
        let endpoint = Url::parse("http://127.0.0.1:9/v1/chat/completions").expect("test URL");
        // Only POST is admissible, so the assertion below proves the span carries
        // the caller's method (propagation) rather than the absence of a
        // hardcoded constant.
        let body = serde_json::json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hello"}],
            "service_tier": "default",
            "n": 1,
            "max_completion_tokens": 16,
        });
        let request = Client::new().post(endpoint.clone()).json(&body);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");
        let authority = DispatchAuthority::runtime_binding(
            "provider-alpha",
            Uuid::nil(),
            None,
            "test-model",
            "http://127.0.0.1:9",
            None,
        );
        let context = authority.attest(ProviderSurface::Chat, None);

        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let _ = send_provider_http_request(
                    request,
                    &context,
                    &crate::integrations::provider_admission::UnqualifiedTestAdmission,
                    "chat",
                )
                .await;
            });
        });

        let output = String::from_utf8(captured.lock().expect("capture lock").clone())
            .expect("span output is utf8");
        assert!(
            output.contains("http.request.method=\"POST\""),
            "the dispatch span must label the caller's method, got: {output}"
        );
        assert!(
            output.contains("provider.operation=\"chat\""),
            "the dispatch span must label the caller's operation, got: {output}"
        );
    }
}

#[cfg(test)]
mod dispatch_attestation_tests {
    use super::*;
    #[test]
    fn job_origin_is_unattributed_without_a_recorded_principal() {
        let mutation_id = Uuid::from_u128(7);
        assert_eq!(
            DispatchCaller::mutation_origin(mutation_id, Some(Uuid::from_u128(9))),
            DispatchCaller::MutationOrigin { mutation_id, principal_id: Uuid::from_u128(9) },
        );
        assert_eq!(
            DispatchCaller::mutation_origin(mutation_id, None),
            DispatchCaller::Unattributed
        );
        assert_eq!(
            DispatchCaller::mutation_origin(mutation_id, Some(Uuid::nil())),
            DispatchCaller::Unattributed,
            "a nil principal is not a caller",
        );
        assert_eq!(
            DispatchCaller::mutation_origin(Uuid::nil(), Some(Uuid::from_u128(9))),
            DispatchCaller::Unattributed,
            "a job without a mutation id names no origin",
        );
    }

    fn context(key: &str) -> ProviderDispatchContext {
        DispatchAuthority::runtime_binding(
            "openai",
            Uuid::nil(),
            None,
            "gpt-6-luna",
            "https://api.openai.com/v1",
            Some(key),
        )
        .attest(
            ProviderSurface::Chat,
            Some((HeaderName::from_static("authorization"), format!("Bearer {key}"))),
        )
    }

    fn built(url: &str, key: &str, body: serde_json::Value) -> reqwest::Request {
        Client::new()
            .post(url)
            .bearer_auth(key)
            .json(&body)
            .build()
            .expect("built provider request")
    }

    #[test]
    fn refuses_a_route_outside_the_attested_base_url() {
        let context = context("key-a");
        let request = built(
            "https://api.openai.com.evil.test/v1/chat/completions",
            "key-a",
            serde_json::json!({}),
        );
        assert!(!context.authorizes_route(request.url()));
    }

    #[test]
    fn refuses_a_credential_that_differs_from_the_attested_digest() {
        let context = context("key-a");
        let request =
            built("https://api.openai.com/v1/chat/completions", "key-b", serde_json::json!({}));
        assert!(context.authorizes_route(request.url()));
        assert!(!context.authorizes_credential(&request));
    }

    #[test]
    fn admits_the_attested_route_and_credential_without_touching_the_body() {
        let context = context("key-a");
        let body = serde_json::json!({
            "model": "gpt-6-luna",
            "messages": [{"role": "user", "content": "hello"}],
            "service_tier": "flex",
            "n": 1,
            "max_completion_tokens": 1024,
        });
        let request = built("https://api.openai.com/v1/chat/completions", "key-a", body.clone());
        let before =
            request.body().and_then(reqwest::Body::as_bytes).expect("in-memory body").to_vec();

        assert!(context.authorizes_route(request.url()));
        assert!(context.authorizes_credential(&request));
        let inspected = inspect_request_body(&request).expect("inspectable body");
        assert_eq!(inspected, body);

        let after =
            request.body().and_then(reqwest::Body::as_bytes).expect("in-memory body").to_vec();
        assert_eq!(before, after, "attestation and inspection must not rewrite the body");
    }

    #[test]
    fn refuses_an_uninspectable_body() {
        let request = Client::new()
            .post("https://api.openai.com/v1/chat/completions")
            .body(String::from("streamed-body-is-not-inspectable"))
            .build()
            .expect("provider request");
        assert!(matches!(
            inspect_request_body(&request),
            Err(ProviderDispatchRefusal::UninspectableBody)
        ));
    }
}
