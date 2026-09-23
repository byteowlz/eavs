//! Network access control for the proxy.
//!
//! Enforces domain allow/deny lists and private IP blocking before
//! the proxy makes upstream requests.

use crate::config::NetworkConfig;
#[cfg(test)]
use crate::config::TrustedPrivateEndpoint;
use std::net::{IpAddr, SocketAddr};

/// Check if a URL is allowed by the network access control policy.
///
/// Returns `Ok(())` if allowed, `Err(reason)` if blocked.
pub fn check_url_allowed(config: &NetworkConfig, url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;

    check_host_allowed(config, host)
}

/// Check a transparent-egress request against both its presented host identity
/// and the actual destination recovered from PROXY protocol.
pub fn check_egress_allowed(
    config: &NetworkConfig,
    host: &str,
    destination: SocketAddr,
) -> Result<(), String> {
    check_host_allowed(config, host)?;

    // An explicitly trusted private endpoint binds host/address/port exactly,
    // so a matching destination needs no further host/destination reconciliation.
    let trusted = config.trusted_private_endpoints.iter().any(|endpoint| {
        endpoint.host.eq_ignore_ascii_case(host)
            && endpoint.address == destination.ip()
            && endpoint.port == destination.port()
    });

    // Bind the presented host identity to the actual destination so an allowed
    // hostname cannot be used to reach a different (e.g. denied) host.
    if !trusted {
        check_host_matches_destination(host, destination)?;
    }

    if config.block_private_ips && is_private_ip(&destination.ip()) && !trusted {
        return Err(format!(
            "Destination '{}' is a private IP address (blocked by network policy)",
            destination
        ));
    }

    Ok(())
}

/// Require the presented host identity to be consistent with the actual PROXY
/// destination. An IP-literal host must equal the destination address; a
/// hostname must resolve to it (fail closed if resolution is unavailable).
fn check_host_matches_destination(host: &str, destination: SocketAddr) -> Result<(), String> {
    let dest_ip = unmap_ip(&destination.ip());

    // Host presented as an IP literal: must equal the destination address.
    if let Ok(host_ip) = host.parse::<IpAddr>() {
        if unmap_ip(&host_ip) != dest_ip {
            return Err(format!(
                "Host '{}' does not match destination '{}'",
                host,
                destination.ip()
            ));
        }
        return Ok(());
    }

    // Hostname: resolve it and require the destination to be one of its
    // addresses. Fail closed on resolution failure so an allowed name cannot
    // conceal a different destination.
    let mut resolved = std::net::ToSocketAddrs::to_socket_addrs(&(host, destination.port()))
        .map_err(|e| format!("Host '{}' could not be resolved: {}", host, e))?;
    if !resolved.any(|a| unmap_ip(&a.ip()) == dest_ip) {
        return Err(format!(
            "Host '{}' does not resolve to destination '{}'",
            host,
            destination.ip()
        ));
    }
    Ok(())
}

/// Check if a host (domain or IP) is allowed by the network access control policy.
pub fn check_host_allowed(config: &NetworkConfig, host: &str) -> Result<(), String> {
    // 1. Check deny list first (highest priority)
    if !config.deny_domains.is_empty() {
        for pattern in &config.deny_domains {
            if glob_match(pattern, host) {
                return Err(format!(
                    "Host '{}' is denied by network policy (matches '{}')",
                    host, pattern
                ));
            }
        }
    }

    // 2. Check private IP blocking
    if config.block_private_ips {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_private_ip(&ip) {
                return Err(format!(
                    "Host '{}' is a private IP address (blocked by network policy)",
                    host
                ));
            }
        }
        // Also check common private hostnames
        if host == "localhost" || host.ends_with(".local") || host.ends_with(".internal") {
            return Err(format!(
                "Host '{}' resolves to a private address (blocked by network policy)",
                host
            ));
        }
    }

    // 3. Check allow list (if non-empty, host MUST match)
    if !config.allow_domains.is_empty() {
        let allowed = config
            .allow_domains
            .iter()
            .any(|pattern| glob_match(pattern, host));
        if !allowed {
            return Err(format!(
                "Host '{}' is not in the allowed domains list",
                host
            ));
        }
    }

    Ok(())
}

/// Simple glob matching supporting `*` (matches any sequence) and `?` (matches one char).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let text = text.to_lowercase();
    glob_match_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_inner(pattern: &[u8], text: &[u8]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    let mut star_pi = usize::MAX;
    let mut star_ti = 0;

    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == b'?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star_pi = pi;
            star_ti = ti;
            pi += 1;
        } else if star_pi != usize::MAX {
            pi = star_pi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }

    pi == pattern.len()
}

/// Map an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) to its IPv4 counterpart
/// so that private-IP classification and host/destination comparisons treat it
/// as the IPv4 address it really is. Non-mapped addresses are unchanged.
fn unmap_ip(ip: &IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(*v6),
        },
        _ => *ip,
    }
}

/// Check if an IP address is in a private/reserved range.
/// An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is classified as its IPv4
/// counterpart so private-IP blocking holds for every representation.
fn is_private_ip(ip: &IpAddr) -> bool {
    match unmap_ip(ip) {
        IpAddr::V4(v4) => {
            v4.is_loopback()           // 127.0.0.0/8
                || v4.is_private()     // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local()  // 169.254.0.0/16
                || v4.is_unspecified() // 0.0.0.0
                || v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1])
            // CGNAT
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()       // ::1
                || v6.is_unspecified() // ::
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_allow(domains: &[&str]) -> NetworkConfig {
        NetworkConfig {
            allow_domains: domains.iter().map(|s| s.to_string()).collect(),
            deny_domains: vec![],
            trusted_private_endpoints: vec![],
            block_private_ips: true,
        }
    }

    fn config_deny(domains: &[&str]) -> NetworkConfig {
        NetworkConfig {
            allow_domains: vec![],
            deny_domains: domains.iter().map(|s| s.to_string()).collect(),
            trusted_private_endpoints: vec![],
            block_private_ips: false,
        }
    }

    fn config_both(allow: &[&str], deny: &[&str]) -> NetworkConfig {
        NetworkConfig {
            allow_domains: allow.iter().map(|s| s.to_string()).collect(),
            deny_domains: deny.iter().map(|s| s.to_string()).collect(),
            trusted_private_endpoints: vec![],
            block_private_ips: true,
        }
    }

    #[test]
    fn test_empty_config_allows_all() {
        let config = NetworkConfig::default();
        assert!(check_url_allowed(&config, "https://api.openai.com/v1/chat").is_ok());
        assert!(check_url_allowed(&config, "https://api.anthropic.com/v1/messages").is_ok());
    }

    #[test]
    fn test_allow_list_restricts() {
        let config = config_allow(&["api.openai.com", "*.anthropic.com"]);
        assert!(check_url_allowed(&config, "https://api.openai.com/v1/chat").is_ok());
        assert!(check_url_allowed(&config, "https://api.anthropic.com/v1/messages").is_ok());
        assert!(check_url_allowed(&config, "https://evil.com/steal").is_err());
    }

    #[test]
    fn test_deny_list_blocks() {
        let config = config_deny(&["evil.com", "*.malware.net"]);
        assert!(check_url_allowed(&config, "https://api.openai.com/v1/chat").is_ok());
        assert!(check_url_allowed(&config, "https://evil.com/steal").is_err());
        assert!(check_url_allowed(&config, "https://sub.malware.net/c2").is_err());
    }

    #[test]
    fn test_deny_takes_precedence() {
        let config = config_both(&["*.openai.com", "evil.openai.com"], &["evil.openai.com"]);
        assert!(check_url_allowed(&config, "https://api.openai.com/v1/chat").is_ok());
        assert!(check_url_allowed(&config, "https://evil.openai.com/bad").is_err());
    }

    #[test]
    fn test_private_ip_blocking() {
        let config = NetworkConfig {
            block_private_ips: true,
            ..Default::default()
        };
        assert!(check_url_allowed(&config, "http://127.0.0.1:8080/api").is_err());
        assert!(check_url_allowed(&config, "http://10.0.0.1/internal").is_err());
        assert!(check_url_allowed(&config, "http://192.168.1.1/admin").is_err());
        assert!(check_url_allowed(&config, "http://localhost:3000/api").is_err());
        assert!(check_url_allowed(&config, "https://api.openai.com/v1/chat").is_ok());
    }

    #[test]
    fn test_private_ip_blocking_disabled() {
        let config = NetworkConfig {
            block_private_ips: false,
            ..Default::default()
        };
        assert!(check_url_allowed(&config, "http://127.0.0.1:8080/api").is_ok());
        assert!(check_url_allowed(&config, "http://localhost:3000/api").is_ok());
    }

    #[test]
    fn test_glob_matching() {
        assert!(glob_match("*.openai.com", "api.openai.com"));
        assert!(glob_match("*.openai.com", "sub.api.openai.com"));
        assert!(!glob_match("*.openai.com", "openai.com"));
        assert!(glob_match("api.openai.com", "api.openai.com"));
        assert!(glob_match("api.openai.com", "API.OPENAI.COM")); // case insensitive
        assert!(glob_match("10.*", "10.0.0.1"));
        assert!(glob_match("172.1?.0.*", "172.16.0.1"));
        assert!(!glob_match("172.1?.0.*", "172.20.0.1"));
    }

    #[test]
    fn test_host_check() {
        let config = config_allow(&["api.openai.com"]);
        assert!(check_host_allowed(&config, "api.openai.com").is_ok());
        assert!(check_host_allowed(&config, "evil.com").is_err());
    }

    fn trusted_zgx() -> NetworkConfig {
        NetworkConfig {
            allow_domains: vec!["zgx".into()],
            trusted_private_endpoints: vec![TrustedPrivateEndpoint {
                host: "zgx".into(),
                address: "100.64.0.18".parse().unwrap(),
                port: 8080,
            }],
            block_private_ips: true,
            ..Default::default()
        }
    }

    #[test]
    fn exact_trusted_private_endpoint_is_allowed() {
        let config = trusted_zgx();
        let destination = "100.64.0.18:8080".parse().unwrap();
        assert!(check_egress_allowed(&config, "zgx", destination).is_ok());
    }

    #[test]
    fn trusted_endpoint_does_not_authorise_adjacent_targets() {
        let config = trusted_zgx();
        assert!(check_egress_allowed(&config, "zgx", "100.64.0.18:8081".parse().unwrap()).is_err());
        assert!(check_egress_allowed(&config, "zgx", "100.64.0.19:8080".parse().unwrap()).is_err());
        assert!(
            check_egress_allowed(&config, "other", "100.64.0.18:8080".parse().unwrap()).is_err()
        );
    }

    #[test]
    fn allowed_public_host_cannot_conceal_private_destination() {
        let config = config_allow(&["api.openai.com"]);
        let destination = "127.0.0.1:8080".parse().unwrap();
        assert!(check_egress_allowed(&config, "api.openai.com", destination).is_err());
    }

    #[test]
    fn deny_rule_overrides_trusted_private_endpoint() {
        let mut config = trusted_zgx();
        config.deny_domains = vec!["zgx".into()];
        let destination = "100.64.0.18:8080".parse().unwrap();
        assert!(check_egress_allowed(&config, "zgx", destination).is_err());
    }

    #[test]
    fn ipv4_mapped_ipv6_private_is_classified_private() {
        // ::ffff:10.0.0.1 and ::ffff:192.168.1.1 encode private IPv4 addresses
        // via the IPv4-mapped IPv6 representation; they must be classified as
        // private so private-IP blocking holds.
        assert!(is_private_ip(&"::ffff:10.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip(
            &"::ffff:192.168.1.1".parse::<IpAddr>().unwrap()
        ));
        assert!(is_private_ip(
            &"::ffff:127.0.0.1".parse::<IpAddr>().unwrap()
        ));
        // A non-mapped global IPv6 address is not private.
        assert!(!is_private_ip(&"2001:db8::1".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn ipv4_mapped_ipv6_private_destination_blocked() {
        // An allowed public host cannot conceal an IPv4-mapped IPv6 private
        // destination: check_egress_allowed must reject it.
        let config = config_allow(&["api.openai.com"]);
        let dest: SocketAddr = "[::ffff:10.0.0.1]:8080".parse().unwrap();
        assert!(check_egress_allowed(&config, "api.openai.com", dest).is_err());
    }

    #[test]
    fn allowed_hostname_to_mismatched_destination_is_rejected() {
        // The presented host is allowed, but the real PROXY destination is a
        // different (here: another public) host. The host->destination binding
        // must reject it so an allowed name cannot reach an arbitrary host.
        let config = config_allow(&["api.openai.com"]);
        let dest: SocketAddr = "8.8.8.8:443".parse().unwrap();
        assert!(check_egress_allowed(&config, "api.openai.com", dest).is_err());
    }

    #[test]
    fn allowed_ip_host_to_mismatched_destination_is_rejected() {
        // Deterministic (no DNS): an allowed IP-literal host must equal the
        // destination; a different destination is rejected.
        let config = config_allow(&["1.2.3.4"]);
        let dest: SocketAddr = "5.6.7.8:443".parse().unwrap();
        assert!(check_egress_allowed(&config, "1.2.3.4", dest).is_err());
    }
}
