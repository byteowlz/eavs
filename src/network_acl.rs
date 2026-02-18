//! Network access control for the proxy.
//!
//! Enforces domain allow/deny lists and private IP blocking before
//! the proxy makes upstream requests.

use crate::config::NetworkConfig;
use std::net::IpAddr;

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

/// Check if an IP address is in a private/reserved range.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()           // 127.0.0.0/8
                || v4.is_private()     // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local()  // 169.254.0.0/16
                || v4.is_unspecified() // 0.0.0.0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()       // ::1
                || v6.is_unspecified() // ::
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
            block_private_ips: true,
        }
    }

    fn config_deny(domains: &[&str]) -> NetworkConfig {
        NetworkConfig {
            allow_domains: vec![],
            deny_domains: domains.iter().map(|s| s.to_string()).collect(),
            block_private_ips: false,
        }
    }

    fn config_both(allow: &[&str], deny: &[&str]) -> NetworkConfig {
        NetworkConfig {
            allow_domains: allow.iter().map(|s| s.to_string()).collect(),
            deny_domains: deny.iter().map(|s| s.to_string()).collect(),
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
}
