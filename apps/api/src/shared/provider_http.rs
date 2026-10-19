use std::{
    collections::VecDeque,
    fmt,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use reqwest::{Client, Method, RequestBuilder, Url};
use thiserror::Error;
use tracing::Instrument as _;

use super::outbound_http::{PublicHttpUrlError, is_non_public_ip};

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
    endpoint: &Url,
    method: &Method,
    provider_kind: &str,
    operation: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    let server_address = endpoint.host_str().unwrap_or("unknown");
    let provider_call_id = crate::observability::current_provider_call_id().unwrap_or_default();
    let span = tracing::info_span!(
        "provider.http.request",
        otel.kind = "client",
        "http.request.method" = method.as_str(),
        "server.address" = server_address,
        "provider.kind" = provider_kind,
        "provider.operation" = operation,
        "provider.call.id" = provider_call_id,
        "http.response.status_code" = tracing::field::Empty,
    );
    let response = async move { crate::observability::inject_trace_context(request).send().await }
        .instrument(span.clone())
        .await;
    if let Ok(response) = &response {
        span.record("http.response.status_code", response.status().as_u16());
    }
    response
}

#[cfg(test)]
mod tests {
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
        let endpoint = Url::parse("http://127.0.0.1:9/v1/models").expect("test URL");
        let request = Client::new().get(endpoint.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");

        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let _ = send_provider_http_request(
                    request,
                    &endpoint,
                    &Method::GET,
                    "provider-alpha",
                    "model-list",
                )
                .await;
            });
        });

        let output = String::from_utf8(captured.lock().expect("capture lock").clone())
            .expect("span output is utf8");
        assert!(
            output.contains("http.request.method=\"GET\""),
            "the dispatch span must label the caller's method, got: {output}"
        );
        assert!(
            !output.contains("http.request.method=\"POST\""),
            "the dispatch span must not hardcode POST, got: {output}"
        );
    }
}
