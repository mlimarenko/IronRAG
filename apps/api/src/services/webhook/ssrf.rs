/// SSRF protection for outbound webhook target URLs.
///
/// Validates that a `target_url` is safe to use as a webhook delivery endpoint:
/// - Scheme must be `https://` (or `http://` when `IRONRAG_WEBHOOK_ALLOW_HTTP=1`)
/// - Hostname must resolve to at least one public, non-private IP address
/// - All resolved addresses must be non-private (any private address is rejected)
///
/// Private ranges covered:
///   IPv4: loopback (127/8), private (10/8, 172.16/12, 192.168/16),
///         link-local (169.254/16), CGNAT (100.64/10)
///   IPv6: loopback (::1), unique-local (fc00::/7)
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Validates `target_url` against SSRF risks.
///
/// Returns `Ok(())` when the URL is safe, `Err(reason)` with a human-readable
/// message describing why the URL was rejected.
///
/// # Errors
/// Returns a `String` describing the rejection reason.
pub async fn validate_target_url(target_url: &str) -> Result<(), String> {
    let allow_http = std::env::var("IRONRAG_WEBHOOK_ALLOW_HTTP").map(|v| v == "1").unwrap_or(false);

    // Parse URL and check scheme.
    let url =
        url::Url::parse(target_url).map_err(|e| format!("target_url is not a valid URL: {e}"))?;

    match url.scheme() {
        "https" => {}
        "http" if allow_http => {}
        "http" => {
            return Err(
                "target_url must use https:// (set IRONRAG_WEBHOOK_ALLOW_HTTP=1 for tests)"
                    .to_string(),
            );
        }
        scheme => {
            return Err(format!("target_url scheme '{scheme}' is not allowed; use https://"));
        }
    }

    let host = url.host_str().ok_or_else(|| "target_url has no host".to_string())?;

    let port = url.port_or_known_default().unwrap_or(443);

    // Resolve hostname and reject if any resolved address is private.
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("target_url host '{host}' could not be resolved: {e}"))?
        .collect();

    if addrs.is_empty() {
        return Err(format!("target_url host '{host}' resolved to no addresses"));
    }

    for addr in &addrs {
        if is_private_ip(addr.ip()) {
            return Err(format!(
                "target_url host '{host}' resolves to a private/reserved address ({}) - \
                 delivery to internal network targets is not allowed",
                addr.ip()
            ));
        }
    }

    Ok(())
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_ipv4(v4),
        IpAddr::V6(v6) => is_private_ipv6(v6),
    }
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()          // 127.0.0.0/8
        || ip.is_private()    // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local() // 169.254/16
        || is_cgnat(ip)       // 100.64.0.0/10 (RFC 6598)
        || ip.is_unspecified()
        || ip.is_broadcast()
}

/// Returns true if `ip` is in the CGNAT range 100.64.0.0/10 (RFC 6598).
fn is_cgnat(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    // 100.64.0.0-100.127.255.255: first octet 100, second octet 64-127
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

fn is_private_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() {
        return true; // ::1
    }
    // fc00::/7 unique-local addresses (ULA)
    let segments = ip.segments();
    (segments[0] & 0xfe00) == 0xfc00
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ipv4_loopback() {
        assert!(is_private_ipv4("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn private_ipv4_rfc1918() {
        assert!(is_private_ipv4("10.0.0.1".parse().unwrap()));
        assert!(is_private_ipv4("172.16.0.1".parse().unwrap()));
        assert!(is_private_ipv4("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn private_ipv4_link_local() {
        assert!(is_private_ipv4("169.254.1.1".parse().unwrap()));
    }

    #[test]
    fn private_ipv4_cgnat() {
        assert!(is_private_ipv4("100.64.0.1".parse().unwrap()));
        assert!(is_private_ipv4("100.127.255.255".parse().unwrap()));
        assert!(!is_private_ipv4("100.128.0.1".parse().unwrap()));
    }

    #[test]
    fn public_ipv4() {
        assert!(!is_private_ipv4("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ipv4("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn private_ipv6_loopback() {
        assert!(is_private_ipv6("::1".parse().unwrap()));
    }

    #[test]
    fn private_ipv6_ula() {
        assert!(is_private_ipv6("fc00::1".parse().unwrap()));
        assert!(is_private_ipv6("fd00::1".parse().unwrap()));
    }

    #[test]
    fn public_ipv6() {
        assert!(!is_private_ipv6("2001:db8::1".parse().unwrap()));
    }
}
