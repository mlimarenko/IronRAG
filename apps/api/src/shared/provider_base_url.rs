use std::{fs, net::Ipv4Addr};

use reqwest::Url;

pub fn provider_base_url_candidates(provider_kind: &str, base_url: &str) -> Vec<String> {
    let normalized = base_url.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut candidates = vec![normalized.clone()];
    if let Some(container_candidate) = docker_host_gateway_candidate(provider_kind, &normalized)
        .filter(|candidate| candidate != &normalized)
    {
        candidates.push(container_candidate);
    }
    candidates
}

pub fn resolve_runtime_provider_base_url(provider_kind: &str, base_url: &str) -> String {
    provider_base_url_candidates(provider_kind, base_url)
        .into_iter()
        .last()
        .unwrap_or_else(|| base_url.trim().trim_end_matches('/').to_string())
}

fn docker_host_gateway_candidate(provider_kind: &str, base_url: &str) -> Option<String> {
    if provider_kind != "ollama" || !running_in_docker() {
        return None;
    }

    rewrite_loopback_url_host(
        base_url,
        parse_default_gateway_ipv4(&fs::read_to_string("/proc/net/route").ok()?)?,
    )
}

fn running_in_docker() -> bool {
    fs::metadata("/.dockerenv").is_ok()
}

fn rewrite_loopback_url_host(base_url: &str, host_gateway: Ipv4Addr) -> Option<String> {
    let mut url = Url::parse(base_url).ok()?;
    let is_loopback = match url.host()? {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(host) => host.is_loopback(),
        url::Host::Ipv6(host) => host.is_loopback(),
    };
    if !is_loopback {
        return None;
    }
    url.set_host(Some(&host_gateway.to_string())).ok()?;
    Some(url.to_string().trim_end_matches('/').to_string())
}

fn parse_default_gateway_ipv4(route_table: &str) -> Option<Ipv4Addr> {
    route_table.lines().skip(1).find_map(|line| {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.get(1).copied() != Some("00000000") {
            return None;
        }
        let gateway_hex = *columns.get(2)?;
        let gateway = u32::from_str_radix(gateway_hex, 16).ok()?;
        Some(Ipv4Addr::from(gateway.to_le_bytes()))
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_default_gateway_ipv4, rewrite_loopback_url_host};
    use std::net::Ipv4Addr;

    #[test]
    fn parses_default_gateway_from_proc_net_route() {
        let route_table = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\neth0\t00000000\t010013AC\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";
        assert_eq!(parse_default_gateway_ipv4(route_table), Some(Ipv4Addr::new(172, 19, 0, 1)));
    }

    #[test]
    fn rewrites_loopback_hosts_to_gateway() {
        assert_eq!(
            rewrite_loopback_url_host("http://localhost:11434/v1", Ipv4Addr::new(172, 19, 0, 1)),
            Some("http://172.19.0.1:11434/v1".to_string())
        );
        assert_eq!(
            rewrite_loopback_url_host("http://127.0.0.1:11434/v1", Ipv4Addr::new(172, 19, 0, 1)),
            Some("http://172.19.0.1:11434/v1".to_string())
        );
    }

    #[test]
    fn leaves_non_loopback_hosts_unchanged() {
        assert_eq!(
            rewrite_loopback_url_host(
                "http://host.docker.internal:11434/v1",
                Ipv4Addr::new(172, 19, 0, 1)
            ),
            None
        );
    }
}
