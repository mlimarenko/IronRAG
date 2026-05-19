use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use futures::StreamExt;
use reqwest::{Client, Url};

#[derive(Debug, Clone)]
pub struct ResolvedPublicHttpUrl {
    url: Url,
    host: String,
    addresses: Vec<SocketAddr>,
}

impl ResolvedPublicHttpUrl {
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicHttpUrlError {
    InvalidUrl(String),
    HttpsRequired,
    ForbiddenScheme(String),
    MissingHost,
    MissingPort,
    ResolveFailed(String),
    NoAddresses,
    NonPublicAddress(IpAddr),
    ClientBuildFailed(String),
    RequestFailed(String),
    RedirectMissingLocation,
    RedirectInvalidLocation(String),
    RedirectInvalidUrl(String),
    RedirectLimitExceeded(usize),
    ContentLengthExceeded { content_length: u64, max_bytes: u64 },
    BodyReadFailed(String),
    BodySizeExceeded { max_bytes: u64 },
}

impl PublicHttpUrlError {
    #[must_use]
    pub const fn is_https_required(&self) -> bool {
        matches!(self, Self::HttpsRequired)
    }

    #[must_use]
    pub const fn is_terminal_policy_rejection(&self) -> bool {
        matches!(
            self,
            Self::InvalidUrl(_)
                | Self::HttpsRequired
                | Self::ForbiddenScheme(_)
                | Self::MissingHost
                | Self::MissingPort
                | Self::NonPublicAddress(_)
        )
    }
}

impl fmt::Display for PublicHttpUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(error) => write!(formatter, "is not a valid URL: {error}"),
            Self::HttpsRequired => formatter.write_str("must use https://"),
            Self::ForbiddenScheme(scheme) => {
                write!(formatter, "scheme '{scheme}' is not allowed; use http(s)")
            }
            Self::MissingHost => formatter.write_str("is missing a host component"),
            Self::MissingPort => formatter.write_str("is missing a resolvable port"),
            Self::ResolveFailed(error) => {
                write!(formatter, "host could not be resolved: {error}")
            }
            Self::NoAddresses => formatter.write_str("host resolved to no addresses"),
            Self::NonPublicAddress(address) => {
                write!(formatter, "resolves to a non-public address ({address})")
            }
            Self::ClientBuildFailed(error) => {
                write!(formatter, "failed to build outbound HTTP client: {error}")
            }
            Self::RequestFailed(error) => write!(formatter, "outbound request failed: {error}"),
            Self::RedirectMissingLocation => {
                formatter.write_str("redirect response is missing Location header")
            }
            Self::RedirectInvalidLocation(error) => {
                write!(formatter, "redirect Location header is invalid: {error}")
            }
            Self::RedirectInvalidUrl(error) => {
                write!(formatter, "redirect Location URL is invalid: {error}")
            }
            Self::RedirectLimitExceeded(limit) => {
                write!(formatter, "outbound request exceeded redirect limit {limit}")
            }
            Self::ContentLengthExceeded { content_length, max_bytes } => {
                write!(
                    formatter,
                    "Content-Length {content_length} exceeds response cap {max_bytes}"
                )
            }
            Self::BodyReadFailed(error) => {
                write!(formatter, "failed to read response body: {error}")
            }
            Self::BodySizeExceeded { max_bytes } => {
                write!(formatter, "response body exceeds cap {max_bytes}")
            }
        }
    }
}

impl std::error::Error for PublicHttpUrlError {}

/// Parses and resolves an outbound HTTP(S) URL, rejecting any target that can
/// resolve back into local, private, link-local, multicast, or CGNAT space.
///
/// The resolved address list is returned so callers can pin the subsequent
/// `reqwest` request to the same validated addresses and avoid DNS rebinding.
///
/// # Errors
/// Returns [`PublicHttpUrlError`] when the URL is invalid, uses a forbidden
/// scheme, cannot be resolved, or resolves to a non-public address.
pub async fn resolve_public_http_url(
    raw_url: &str,
    allow_http: bool,
) -> Result<ResolvedPublicHttpUrl, PublicHttpUrlError> {
    let parsed = Url::parse(raw_url.trim())
        .map_err(|error| PublicHttpUrlError::InvalidUrl(error.to_string()))?;
    match parsed.scheme() {
        "https" => {}
        "http" if allow_http => {}
        "http" => return Err(PublicHttpUrlError::HttpsRequired),
        scheme => return Err(PublicHttpUrlError::ForbiddenScheme(scheme.to_string())),
    }

    let host = parsed.host_str().ok_or(PublicHttpUrlError::MissingHost)?.to_string();
    let port = parsed.port_or_known_default().ok_or(PublicHttpUrlError::MissingPort)?;
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|error| PublicHttpUrlError::ResolveFailed(error.to_string()))?
        .collect();
    if addresses.is_empty() {
        return Err(PublicHttpUrlError::NoAddresses);
    }
    for address in &addresses {
        if is_non_public_ip(address.ip()) {
            return Err(PublicHttpUrlError::NonPublicAddress(address.ip()));
        }
    }

    Ok(ResolvedPublicHttpUrl { url: parsed, host, addresses })
}

/// Builds a no-redirect HTTP client pinned to addresses returned by
/// [`resolve_public_http_url`].
///
/// # Errors
/// Returns the underlying [`reqwest::Error`] when the client cannot be built.
pub fn build_no_redirect_public_http_client(
    resolved: &ResolvedPublicHttpUrl,
    timeout: Duration,
    connect_timeout: Duration,
    user_agent: Option<&str>,
) -> Result<Client, reqwest::Error> {
    let mut builder = Client::builder()
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(&resolved.host, &resolved.addresses);
    if let Some(user_agent) = user_agent.filter(|value| !value.trim().is_empty()) {
        builder = builder.user_agent(user_agent.to_string());
    }
    builder.build()
}

/// Sends a GET request to a public HTTP(S) URL and follows redirects manually,
/// validating and DNS-pinning every hop before connecting.
///
/// # Errors
/// Returns [`PublicHttpUrlError`] when any hop is invalid, resolves to a
/// non-public address, fails to connect, or exceeds `max_redirects`.
pub async fn get_public_http_following_redirects(
    initial_url: &str,
    allow_http: bool,
    max_redirects: usize,
    timeout: Duration,
    connect_timeout: Duration,
    user_agent: Option<&str>,
) -> Result<reqwest::Response, PublicHttpUrlError> {
    let mut current_url = Url::parse(initial_url.trim())
        .map_err(|error| PublicHttpUrlError::InvalidUrl(error.to_string()))?;
    for redirect_count in 0..=max_redirects {
        let resolved = resolve_public_http_url(current_url.as_str(), allow_http).await?;
        let client =
            build_no_redirect_public_http_client(&resolved, timeout, connect_timeout, user_agent)
                .map_err(|error| PublicHttpUrlError::ClientBuildFailed(error.to_string()))?;
        let response =
            crate::observability::inject_trace_context(client.get(resolved.url().clone()))
                .send()
                .await
                .map_err(|error| PublicHttpUrlError::RequestFailed(error.to_string()))?;
        if !response.status().is_redirection() {
            return Ok(response);
        }
        if redirect_count >= max_redirects {
            return Err(PublicHttpUrlError::RedirectLimitExceeded(max_redirects));
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .ok_or(PublicHttpUrlError::RedirectMissingLocation)?
            .to_str()
            .map_err(|error| PublicHttpUrlError::RedirectInvalidLocation(error.to_string()))?;
        current_url = resolve_redirect_target_url(resolved.url(), location, allow_http).await?;
    }
    Err(PublicHttpUrlError::RedirectLimitExceeded(max_redirects))
}

/// Resolves a redirect `Location` relative to `base_url` and validates that the
/// next hop is public before the caller attempts to connect to it.
///
/// # Errors
/// Returns [`PublicHttpUrlError`] when the redirect location is malformed or
/// resolves to a forbidden/non-public target.
pub async fn resolve_redirect_target_url(
    base_url: &Url,
    location: &str,
    allow_http: bool,
) -> Result<Url, PublicHttpUrlError> {
    let next_url = base_url
        .join(location)
        .map_err(|error| PublicHttpUrlError::RedirectInvalidUrl(error.to_string()))?;
    resolve_public_http_url(next_url.as_str(), allow_http).await?;
    Ok(next_url)
}

/// Reads a response body into memory while enforcing `max_bytes` both through
/// `Content-Length` and while streaming chunked bodies.
///
/// # Errors
/// Returns [`PublicHttpUrlError`] when the response advertises or streams more
/// than `max_bytes`, or when the transport fails while reading.
pub async fn read_response_bytes_with_limit(
    response: reqwest::Response,
    max_bytes: u64,
) -> Result<Vec<u8>, PublicHttpUrlError> {
    if let Some(content_length) = response.content_length() {
        if content_length > max_bytes {
            return Err(PublicHttpUrlError::ContentLengthExceeded { content_length, max_bytes });
        }
    }
    let mut buffer = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk_result) = stream.next().await {
        let chunk =
            chunk_result.map_err(|error| PublicHttpUrlError::BodyReadFailed(error.to_string()))?;
        let next_len = (buffer.len() as u64).saturating_add(chunk.len() as u64);
        if next_len > max_bytes {
            return Err(PublicHttpUrlError::BodySizeExceeded { max_bytes });
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer)
}

/// Reads at most `max_bytes` from a response and returns a character excerpt.
///
/// # Errors
/// Returns [`PublicHttpUrlError`] on body read or size-limit failures.
pub async fn read_response_text_excerpt_with_limit(
    response: reqwest::Response,
    max_bytes: u64,
    excerpt_chars: usize,
) -> Result<String, PublicHttpUrlError> {
    let bytes = read_response_bytes_with_limit(response, max_bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).chars().take(excerpt_chars).collect())
}

#[must_use]
pub fn is_non_public_ip(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_non_public_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_non_public_ipv4(mapped);
            }
            is_non_public_ipv6(v6)
        }
    }
}

fn is_non_public_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (octets[1] & 0b1100_0000) == 64)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        || octets[0] >= 240
}

fn is_non_public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let first_segment = segments[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || segments[..6].iter().all(|segment| *segment == 0)
        || (segments[0] == 0x0064
            && segments[1] == 0xff9b
            && segments[2..6].iter().all(|segment| *segment == 0))
        || (first_segment == 0x0100 && segments[1..4] == [0, 0, 0])
        || first_segment == 0x2002
        || (first_segment & 0xffc0) == 0xfec0
        || (first_segment == 0x2001 && segments[1] == 0x0db8)
        || (first_segment & 0xfe00) == 0xfc00
        || (first_segment & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr},
        str::FromStr,
        time::Duration,
    };

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{
        PublicHttpUrlError, ResolvedPublicHttpUrl, build_no_redirect_public_http_client,
        is_non_public_ip, resolve_redirect_target_url,
    };

    #[test]
    fn outbound_guard_blocks_non_public_addresses() {
        let blocked = [
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::from_str("fc00::1").expect("valid ipv6"),
            IpAddr::from_str("fe80::1").expect("valid ipv6"),
            IpAddr::from_str("::ffff:10.0.0.1").expect("valid mapped ipv6"),
            IpAddr::from_str("::8.8.8.8").expect("valid compatible ipv6"),
            IpAddr::from_str("64:ff9b::10.0.0.1").expect("valid nat64 ipv6"),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            IpAddr::V4(Ipv4Addr::new(198, 18, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)),
            IpAddr::from_str("2002::1").expect("valid 6to4 ipv6"),
            IpAddr::from_str("2001:db8::1").expect("valid documentation ipv6"),
        ];
        for addr in blocked {
            assert!(is_non_public_ip(addr), "{addr} should be blocked");
        }

        let allowed = [
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
            IpAddr::from_str("2606:4700:4700::1111").expect("valid ipv6"),
        ];
        for addr in allowed {
            assert!(!is_non_public_ip(addr), "{addr} should be allowed");
        }
    }

    #[tokio::test]
    async fn redirect_target_rejects_private_address() {
        let base_url = reqwest::Url::parse("https://example.com/start").expect("base url");
        let error = resolve_redirect_target_url(&base_url, "http://127.0.0.1/private", true)
            .await
            .expect_err("private redirect should be rejected");

        assert!(matches!(
            error,
            PublicHttpUrlError::NonPublicAddress(IpAddr::V4(address))
                if address == Ipv4Addr::new(127, 0, 0, 1)
        ));
    }

    #[tokio::test]
    async fn redirect_target_rejects_non_http_scheme() {
        let base_url = reqwest::Url::parse("https://example.com/start").expect("base url");
        let error = resolve_redirect_target_url(&base_url, "file:///etc/passwd", true)
            .await
            .expect_err("non-http redirect should be rejected");

        assert!(matches!(error, PublicHttpUrlError::ForbiddenScheme(scheme) if scheme == "file"));
    }

    #[tokio::test]
    async fn outbound_http_client_does_not_follow_redirects() {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind test listener");
        let addr = listener.local_addr().expect("listener address");
        let url = reqwest::Url::parse(&format!("http://127.0.0.1:{}/redirect", addr.port()))
            .expect("test url");
        let resolved =
            ResolvedPublicHttpUrl { url, host: "127.0.0.1".to_string(), addresses: vec![addr] };

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept test request");
            let mut buffer = [0_u8; 1024];
            let _ = socket.read(&mut buffer).await.expect("read test request");
            socket
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/not-followed\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .expect("write redirect response");
        });

        let client = build_no_redirect_public_http_client(
            &resolved,
            Duration::from_secs(5),
            Duration::from_secs(5),
            None,
        )
        .expect("build fetch client");
        let response = client.get(resolved.url().clone()).send().await.expect("send test request");

        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        server.await.expect("test server task");
    }
}
