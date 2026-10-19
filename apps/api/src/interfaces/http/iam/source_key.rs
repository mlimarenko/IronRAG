//! The admission source key shared by the public login entry points.
//!
//! `POST /v1/iam/sessions` (password login) and `GET /v1/iam/oidc/login`
//! (OIDC start) are both unauthenticated and both spend a budget the caller
//! does not control: the first bounds Argon2id work, the second bounds the
//! issuer-wide OIDC start budget. Both need the caller's best-effort network
//! origin, derived the same way, so the two admission controls can never
//! disagree about who a request came from.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{HeaderMap, header, request::Parts},
};
use tracing::warn;

/// The TCP peer address, when the connection was served through
/// `into_make_service_with_connect_info` (production) -- absent under a
/// direct `Router::oneshot` call (every integration test), since that
/// path never populates the `ConnectInfo` extension. `ConnectInfo<T>`
/// itself only implements the required `FromRequestParts`, not the
/// optional variant, so this reads the extension by hand instead of
/// failing extraction -- and therefore the whole request -- whenever it is
/// missing.
pub(crate) struct OptionalConnectInfo(pub(crate) Option<SocketAddr>);

impl<S> FromRequestParts<S> for OptionalConnectInfo
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let peer_addr = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|info| info.0);
        async move { Ok(Self(peer_addr)) }
    }
}

/// Derives the admission source key from the request.
///
/// `X-Forwarded-For` is trustworthy only for the entries appended by proxies
/// this deployment owns; everything to the left of those is client-supplied.
/// `trusted_proxy_hops` is how many owned proxies *append* their peer to the
/// header before the request reaches this process (the bundled web nginx is
/// one; an ingress gateway in front of it is another), so the client address
/// is the entry that many positions from the right. Over-estimating the count
/// is the unsafe direction -- it lets a caller choose its own bucket key with
/// a forged leading entry; under-estimating collapses every caller behind the
/// same proxy onto one bucket. The count is only meaningful when every path to
/// this process crosses exactly that many appending proxies -- on Kubernetes
/// that is a NetworkPolicy fact (web admits only the ingress gateway, the API
/// admits only web and named in-estate workloads), not something this code
/// can verify; a caller that reaches an inner hop directly can pad the header
/// to the expected length and pick its own bucket.
///
/// `0` ignores the header and keys on the TCP peer. The header is likewise
/// ignored when it carries fewer entries than trusted hops -- the request did
/// not traverse the expected proxies -- falling back to the TCP peer and, when
/// that is unavailable too (a `Router::oneshot` test harness), to a single
/// shared bucket that is logged, never a hard failure.
pub(crate) fn extract_login_source_key(
    headers: &HeaderMap,
    connect_info: Option<SocketAddr>,
    trusted_proxy_hops: u32,
) -> String {
    let hops = usize::try_from(trusted_proxy_hops).unwrap_or(usize::MAX);
    if hops > 0 {
        let entries: Vec<&str> = headers
            .get_all(header::HeaderName::from_static("x-forwarded-for"))
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|raw| raw.split(','))
            .map(str::trim)
            .filter(|candidate| !candidate.is_empty())
            .collect();
        if let Some(trusted_hop) =
            entries.len().checked_sub(hops).and_then(|index| entries.get(index))
        {
            return (*trusted_hop).to_string();
        }
    }
    if let Some(addr) = connect_info {
        return addr.ip().to_string();
    }
    warn!(
        "login rate limit could not determine a source key; degrading to the shared fallback bucket"
    );
    "unknown-source".to_string()
}

#[cfg(test)]
mod tests {
    use super::extract_login_source_key;
    use axum::http::{HeaderMap, HeaderValue};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9)), 4000);

    fn forwarded(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn a_forged_leading_hop_never_becomes_the_source_key() {
        // The gateway appended the real client, then web nginx appended the
        // gateway: two trusted hops, and the attacker-chosen first entry is
        // ignored. Rotating it does not move the caller to a new bucket.
        let headers = forwarded("198.51.100.77, 203.0.113.10, 10.42.0.3");
        assert_eq!(extract_login_source_key(&headers, Some(PEER), 2), "203.0.113.10");
        let headers = forwarded("198.51.100.78, 203.0.113.10, 10.42.0.3");
        assert_eq!(extract_login_source_key(&headers, Some(PEER), 2), "203.0.113.10");
    }

    #[test]
    fn exactly_trusted_hops_entries_selects_the_first_and_is_the_documented_boundary() {
        // With exactly `hops` entries the leftmost one is selected: safe only
        // when the request really crossed `hops` appending proxies, which is
        // why the count must match the enforced topology, not exceed it.
        let headers = forwarded("203.0.113.10, 10.42.0.3");
        assert_eq!(extract_login_source_key(&headers, Some(PEER), 2), "203.0.113.10");
    }

    #[test]
    fn multiple_header_lines_are_read_in_order() {
        let mut headers = HeaderMap::new();
        headers.append("x-forwarded-for", HeaderValue::from_static("198.51.100.77"));
        headers.append("x-forwarded-for", HeaderValue::from_static("203.0.113.10, 10.42.0.3"));
        assert_eq!(extract_login_source_key(&headers, Some(PEER), 2), "203.0.113.10");
    }

    #[test]
    fn one_trusted_hop_reads_the_last_entry() {
        let headers = forwarded("198.51.100.77, 203.0.113.10");
        assert_eq!(extract_login_source_key(&headers, Some(PEER), 1), "203.0.113.10");
    }

    #[test]
    fn zero_trusted_hops_ignores_the_header_entirely() {
        let headers = forwarded("203.0.113.10");
        assert_eq!(extract_login_source_key(&headers, Some(PEER), 0), "10.0.0.9");
    }

    #[test]
    fn fewer_entries_than_trusted_hops_falls_back_to_the_peer() {
        // The request did not traverse the expected proxies, so the header
        // cannot be attributed to any of them.
        let headers = forwarded("203.0.113.10");
        assert_eq!(extract_login_source_key(&headers, Some(PEER), 2), "10.0.0.9");
        assert_eq!(extract_login_source_key(&HeaderMap::new(), Some(PEER), 1), "10.0.0.9");
    }

    #[test]
    fn no_header_and_no_peer_degrades_to_the_shared_bucket() {
        assert_eq!(extract_login_source_key(&HeaderMap::new(), None, 1), "unknown-source");
    }
}
