//! How the gateway identifies a conversation and itself to upstream providers.
//!
//! Session affinity: several gateways route and prompt-cache per conversation
//! (`OpenCode` requires `x-opencode-session`, `OpenRouter` uses `x-session-id`).
//! Clients send their own session id in one of a few common headers; the
//! gateway never forwards it raw. It derives an opaque value scoped to the
//! virtual key and provider, so keys never share upstream sessions and client
//! identifiers do not leave the gateway. Without a client session id the value
//! is stable per key (never one global session for everyone).
//!
//! User-Agent: providers ask clients to identify themselves. The default keeps
//! the client's identity visible and appends `eavs/<version>`; operators can
//! override it per provider (`user_agent`, with `{client}` and `{eavs}`).

use http::HeaderMap;
use sha2::{Digest, Sha256};

use crate::provider::ProviderType;

/// Client headers that carry a conversation/session id, in precedence order.
const SESSION_HEADERS: &[&str] = &[
    "x-opencode-session",
    "x-session-affinity",
    "x-session-id",
    "session_id",
    "x-conversation-id",
];

/// The client's session id, if it sent one.
pub fn client_session(headers: &HeaderMap) -> Option<&str> {
    SESSION_HEADERS.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

/// Opaque upstream session id scoped to `key_scope` (the virtual key hash,
/// or a fixed operator scope) and `provider`.
pub fn upstream_session(key_scope: &str, provider: &str, session: Option<&str>) -> String {
    let mut hash = Sha256::new();
    for part in [
        "eavs-session-v1",
        key_scope,
        provider,
        session.unwrap_or(""),
    ] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    format!("eavs-{}", &hex::encode(hash.finalize())[..32])
}

/// Header carrying the session upstream: the provider's configured
/// `session_header` (empty disables), else the provider type's convention.
pub fn session_header(provider_type: ProviderType, configured: Option<&str>) -> Option<String> {
    match configured.map(str::trim) {
        Some("") => None,
        Some(name) => Some(name.to_ascii_lowercase()),
        None => match provider_type {
            ProviderType::OpenCode | ProviderType::OpenCodeGo => {
                Some("x-opencode-session".to_owned())
            }
            ProviderType::OpenRouter => Some("x-session-id".to_owned()),
            _ => None,
        },
    }
}

/// Template placeholder for the client's User-Agent.
pub const CLIENT_PLACEHOLDER: &str = concat!("{", "client", "}");
/// Template placeholder for `eavs/<version>`.
pub const EAVS_PLACEHOLDER: &str = concat!("{", "eavs", "}");

/// `eavs/<version>`.
pub fn eavs_agent() -> String {
    format!("eavs/{}", env!("CARGO_PKG_VERSION"))
}

/// The User-Agent to send upstream. `template` may use `{client}` (the
/// client's User-Agent, empty if none) and `{eavs}`; an empty template sends
/// none. Default: `<client UA> eavs/<version>`, or `eavs/<version>` alone.
pub fn user_agent(template: Option<&str>, client: Option<&str>) -> Option<String> {
    let client = client.map_or("", str::trim);
    let rendered = match template {
        Some(template) => template
            .replace(CLIENT_PLACEHOLDER, client)
            .replace(EAVS_PLACEHOLDER, &eavs_agent()),
        None if client.is_empty() => eavs_agent(),
        None => format!("{client} {}", eavs_agent()),
    };
    let rendered = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    (!rendered.is_empty()).then_some(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, http::HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn client_session_uses_known_headers_in_order() {
        assert_eq!(client_session(&headers(&[])), None);
        assert_eq!(
            client_session(&headers(&[("x-session-affinity", "pi-1")])),
            Some("pi-1")
        );
        assert_eq!(
            client_session(&headers(&[
                ("x-conversation-id", "conv"),
                ("x-opencode-session", "oc")
            ])),
            Some("oc")
        );
        assert_eq!(client_session(&headers(&[("session_id", "  ")])), None);
    }

    #[test]
    fn upstream_session_is_opaque_stable_and_scoped() {
        let a = upstream_session("key-a", "opencode-go", Some("pi-1"));
        assert_eq!(a, upstream_session("key-a", "opencode-go", Some("pi-1")));
        assert!(a.starts_with("eavs-") && a.len() == 37 && !a.contains("pi-1"));
        assert_ne!(
            a,
            upstream_session("key-b", "opencode-go", Some("pi-1")),
            "per key"
        );
        assert_ne!(
            a,
            upstream_session("key-a", "openrouter", Some("pi-1")),
            "per provider"
        );
        assert_ne!(
            a,
            upstream_session("key-a", "opencode-go", Some("pi-2")),
            "per conversation"
        );
        assert_ne!(
            upstream_session("key-a", "p", None),
            upstream_session("key-b", "p", None),
            "no global session without a client id"
        );
    }

    #[test]
    fn session_header_defaults_and_overrides() {
        assert_eq!(
            session_header(ProviderType::OpenCodeGo, None).as_deref(),
            Some("x-opencode-session")
        );
        assert_eq!(
            session_header(ProviderType::OpenRouter, None).as_deref(),
            Some("x-session-id")
        );
        assert_eq!(session_header(ProviderType::OpenAI, None), None);
        assert_eq!(
            session_header(ProviderType::OpenAI, Some("X-Session-Affinity")).as_deref(),
            Some("x-session-affinity")
        );
        assert_eq!(session_header(ProviderType::OpenCodeGo, Some("")), None);
    }

    #[test]
    fn user_agent_defaults_templates_and_disable() {
        let eavs = eavs_agent();
        assert_eq!(user_agent(None, None), Some(eavs.clone()));
        assert_eq!(
            user_agent(None, Some("pi (darwin 25.0; arm64)")),
            Some(format!("pi (darwin 25.0; arm64) {eavs}"))
        );
        let custom = format!("my-agent/1.0 {EAVS_PLACEHOLDER}");
        assert_eq!(
            user_agent(Some(&custom), Some("pi")),
            Some(format!("my-agent/1.0 {eavs}"))
        );
        let default = format!("{CLIENT_PLACEHOLDER} {EAVS_PLACEHOLDER}");
        assert_eq!(user_agent(Some(&default), None), Some(eavs));
        assert_eq!(
            user_agent(Some(CLIENT_PLACEHOLDER), Some("pi/1")),
            Some("pi/1".into())
        );
        assert_eq!(user_agent(Some(""), Some("pi")), None);
    }
}
