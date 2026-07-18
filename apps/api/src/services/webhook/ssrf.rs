/// Validates `target_url` against SSRF risks.
///
/// Returns `Ok(())` when the URL is safe, `Err(reason)` with a human-readable
/// message describing why the URL was rejected.
///
/// # Errors
/// Returns a `String` describing the rejection reason.
pub async fn validate_target_url(target_url: &str) -> Result<(), String> {
    let allow_http = std::env::var("IRONRAG_WEBHOOK_ALLOW_HTTP").is_ok_and(|v| v == "1");
    crate::shared::outbound_http::resolve_public_http_url(target_url, allow_http)
        .await
        .map(|_| ())
        .map_err(|error| {
            if !allow_http && error.is_https_required() {
                "target_url must use https:// (set IRONRAG_WEBHOOK_ALLOW_HTTP=1 for tests)"
                    .to_string()
            } else {
                format!("target_url {error}")
            }
        })
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr},
        str::FromStr,
    };

    #[test]
    fn private_ipv4_loopback() {
        assert!(crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(
            "127.0.0.1".parse().unwrap()
        )));
    }

    #[test]
    fn private_ipv4_rfc1918() {
        assert!(crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(
            "10.0.0.1".parse().unwrap()
        )));
        assert!(crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(
            "172.16.0.1".parse().unwrap()
        )));
        assert!(crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(
            "192.168.1.1".parse().unwrap()
        )));
    }

    #[test]
    fn private_ipv4_link_local() {
        assert!(crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(
            "169.254.1.1".parse().unwrap()
        )));
    }

    #[test]
    fn private_ipv4_cgnat() {
        assert!(crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(
            "100.64.0.1".parse().unwrap()
        )));
        assert!(crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(
            "100.127.255.255".parse().unwrap()
        )));
        assert!(!crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(
            "100.128.0.1".parse().unwrap()
        )));
    }

    #[test]
    fn public_ipv4() {
        assert!(!crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(Ipv4Addr::new(
            8, 8, 8, 8
        ))));
        assert!(!crate::shared::outbound_http::is_non_public_ip(IpAddr::V4(Ipv4Addr::new(
            1, 1, 1, 1
        ))));
    }

    #[test]
    fn private_ipv6_loopback() {
        assert!(crate::shared::outbound_http::is_non_public_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn private_ipv6_ula() {
        assert!(crate::shared::outbound_http::is_non_public_ip(
            IpAddr::from_str("fc00::1").unwrap()
        ));
        assert!(crate::shared::outbound_http::is_non_public_ip(
            IpAddr::from_str("fd00::1").unwrap()
        ));
    }

    #[test]
    fn public_ipv6() {
        assert!(!crate::shared::outbound_http::is_non_public_ip(
            IpAddr::from_str("2606:4700:4700::1111").unwrap()
        ));
    }
}
