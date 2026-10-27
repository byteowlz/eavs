//! Instance registry: which eavs gateways this console manages and how it
//! authenticates to each.
//!
//! An instance is reached through its operator listener (ADR-0006/0007). The
//! console needs two credentials: the listener token (`X-Eavs-Token`, the
//! boundary) and the master key (`Authorization: Bearer`, administration).
//! Both are secret *references* (`env:`, `keychain:`), resolved only when a
//! request is made, never written back or shown.
//!
//! The local gateway is discovered from its own config file so that the
//! console works with zero setup on the machine running eavs. Remote instances
//! (the server) are declared in the console config.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

/// Keychain service shared with eavs itself (`api_key = "keychain:<account>"`).
pub const KEYCHAIN_SERVICE: &str = "eavs";

/// A credential reference. Literal values are accepted only for the local
/// instance discovered from eavs' own config (machine-local trust).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretRef {
    Env(String),
    Keychain(String),
    Literal(String),
}

impl SecretRef {
    /// Parse `env:NAME`, `keychain:account` or a literal value.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        Some(if let Some(name) = raw.strip_prefix("env:") {
            Self::Env(name.to_owned())
        } else if let Some(account) = raw.strip_prefix("keychain:") {
            Self::Keychain(account.to_owned())
        } else {
            Self::Literal(raw.to_owned())
        })
    }

    /// Resolve the secret now. Errors name the reference, never the value.
    pub fn resolve(&self) -> Result<String> {
        let value = match self {
            Self::Env(name) => std::env::var(name)
                .with_context(|| format!("environment variable {name} is not set"))?,
            Self::Keychain(account) => keyring::Entry::new(KEYCHAIN_SERVICE, account)
                .and_then(|entry| entry.get_password())
                .map_err(|_| {
                    anyhow::anyhow!("keychain entry {KEYCHAIN_SERVICE}/{account} is not readable")
                })?,
            Self::Literal(value) => value.clone(),
        };
        if value.trim().is_empty() {
            bail!("credential {} is empty", self.describe());
        }
        Ok(value)
    }

    /// Where the secret lives, for display (never the value).
    pub fn describe(&self) -> String {
        match self {
            Self::Env(name) => format!("env:{name}"),
            Self::Keychain(account) => format!("keychain:{account}"),
            Self::Literal(_) => "eavs config file".to_owned(),
        }
    }
}

/// How an instance was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The gateway on this machine, from its own config.
    Local,
    /// Declared in the console config.
    Declared,
}

/// One manageable eavs gateway.
#[derive(Debug, Clone)]
pub struct Instance {
    pub name: String,
    /// Operator listener base URL, e.g. `http://127.0.0.1:3044`.
    pub url: String,
    pub token: Option<SecretRef>,
    pub master_key: Option<SecretRef>,
    pub origin: Origin,
}

/// Declared instance in `~/.config/eavs-console/config.toml`:
///
/// ```toml
/// [[instances]]
/// name = "server"
/// url = "http://127.0.0.1:13044"          # loopback SSH tunnel or https:// over the tailnet
/// token = "keychain:server-listener-token"
/// master_key = "keychain:server-master-key"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclaredInstance {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_key: Option<String>,
}

impl DeclaredInstance {
    /// Validate and turn into an instance. Declared credentials must be
    /// references: a literal secret in the console config is refused.
    pub fn into_instance(self) -> Result<Instance> {
        validate_url(&self.url)?;
        let reference = |field: &str, raw: Option<String>| -> Result<Option<SecretRef>> {
            match raw.as_deref().and_then(SecretRef::parse) {
                Some(SecretRef::Literal(_)) => bail!(
                    "instance {}: {field} must be a reference (env:NAME or keychain:account), not a literal secret",
                    self.name
                ),
                other => Ok(other),
            }
        };
        Ok(Instance {
            token: reference("token", self.token.clone())?,
            master_key: reference("master_key", self.master_key.clone())?,
            name: self.name,
            url: self.url.trim_end_matches('/').to_owned(),
            origin: Origin::Declared,
        })
    }
}

/// HTTPS everywhere except loopback (an SSH tunnel or the local gateway).
/// URL credentials, queries and fragments are refused.
pub fn validate_url(raw: &str) -> Result<()> {
    let url = url::Url::parse(raw).with_context(|| format!("invalid URL {raw:?}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("instance URL must not carry credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("instance URL must not have a query or fragment");
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(&url) => Ok(()),
        "http" => bail!("plain HTTP is only allowed to loopback (use an SSH tunnel or HTTPS)"),
        other => bail!("unsupported scheme {other}"),
    }
}

fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// The subset of eavs' own config the console reads to find the local
/// gateway's operator listener and credentials.
#[derive(Debug, Default, Deserialize)]
struct EavsConfig {
    #[serde(default)]
    server: EavsServer,
    #[serde(default)]
    keys: EavsKeys,
}

#[derive(Debug, Default, Deserialize)]
struct EavsServer {
    port: Option<u16>,
    auth_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct EavsKeys {
    master_key: Option<String>,
}

/// eavs' config path: `$XDG_CONFIG_HOME/eavs/config.toml` or `~/.config/eavs/config.toml`.
pub fn eavs_config_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|root| root.join("eavs").join("config.toml"))
}

/// The local gateway, if eavs is configured on this machine.
pub fn discover_local(path: &Path) -> Option<Instance> {
    let text = std::fs::read_to_string(path).ok()?;
    let config: EavsConfig = toml::from_str(&text).ok()?;
    let port = config.server.port.unwrap_or(3033);
    // eavs serves its operator API on the configured port; a split inference
    // listener (ADR-0020) keeps the operator listener on the same port.
    let token = std::env::var("EAVS_AUTH_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|_| SecretRef::Env("EAVS_AUTH_TOKEN".to_owned()))
        .or_else(|| {
            config
                .server
                .auth_token
                .as_deref()
                .and_then(SecretRef::parse)
        });
    Some(Instance {
        name: "This machine".to_owned(),
        url: format!("http://127.0.0.1:{port}"),
        token,
        master_key: config.keys.master_key.as_deref().and_then(SecretRef::parse),
        origin: Origin::Local,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_instances_refuse_literal_secrets_and_plain_http() {
        let ok = DeclaredInstance {
            name: "server".into(),
            url: "https://eavs.example.ts.net/".into(),
            token: Some("keychain:server-token".into()),
            master_key: Some("env:SERVER_MASTER".into()),
        };
        let instance = ok.clone().into_instance().unwrap();
        assert_eq!(instance.url, "https://eavs.example.ts.net");
        assert_eq!(
            instance.token,
            Some(SecretRef::Keychain("server-token".into()))
        );

        let literal = DeclaredInstance {
            master_key: Some("not-a-reference-literal".into()),
            ..ok.clone()
        };
        assert!(literal.into_instance().is_err());

        let cleartext = DeclaredInstance {
            url: "http://100.100.100.1:3044".into(),
            ..ok.clone()
        };
        assert!(cleartext.into_instance().is_err());

        let tunnel = DeclaredInstance {
            url: "http://127.0.0.1:13044".into(),
            ..ok
        };
        assert!(tunnel.into_instance().is_ok());
        assert!(validate_url("https://user:pw@host").is_err());
    }

    #[test]
    fn local_gateway_is_discovered_from_eavs_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[server]\nport = 3044\nauth_token = \"keychain:listener\"\n[keys]\nmaster_key = \"env:EAVS_MASTER_KEY_TEST\"\n",
        )
        .unwrap();
        let local = discover_local(&path).unwrap();
        assert_eq!(local.url, "http://127.0.0.1:3044");
        assert_eq!(local.origin, Origin::Local);
        assert_eq!(
            local.master_key,
            Some(SecretRef::Env("EAVS_MASTER_KEY_TEST".into()))
        );
        assert!(discover_local(&dir.path().join("missing.toml")).is_none());
    }

    #[test]
    fn secret_errors_never_contain_the_value() {
        let missing = SecretRef::Env("EAVS_CONSOLE_DEFINITELY_UNSET".into());
        let error = missing.resolve().unwrap_err().to_string();
        assert!(error.contains("EAVS_CONSOLE_DEFINITELY_UNSET"));
        assert_eq!(
            SecretRef::Literal("s3cret".into()).describe(),
            "eavs config file"
        );
    }
}
