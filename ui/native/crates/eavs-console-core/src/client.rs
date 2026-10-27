//! Typed client for one instance's operator API.
//!
//! Blocking by design: the app runs every call on a background executor.
//! Transport follows ADR-0007: no redirects, no ambient proxy, bounded
//! response size and time; errors are classified locally and never echo a
//! credential.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::instances::Instance;

const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: u64 = 4 << 20;

/// Why a call failed, in terms an operator can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The gateway did not answer (down, wrong URL, tunnel closed).
    Unreachable(String),
    /// A credential is missing, unreadable or rejected.
    Unauthorized(String),
    /// The gateway refused the operation (conflict, validation, not found).
    Rejected { status: u16, message: String },
    /// The response was not what the API documents.
    Protocol(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(m) | Self::Unauthorized(m) | Self::Protocol(m) => f.write_str(m),
            Self::Rejected { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for ApiError {}

/// A provider as reported by `GET /admin/providers/sources`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProviderSource {
    pub name: String,
    pub provider_type: String,
    /// `toml` (config file, read-only here) or `admin` (managed at runtime).
    pub source: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

impl ProviderSource {
    pub fn managed(&self) -> bool {
        self.source != "toml"
    }
}

/// A model in a provider's universe.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ModelRef {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// A provider in the availability snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProviderModels {
    pub provider: String,
    #[serde(rename = "type", default)]
    pub provider_type: String,
    #[serde(default)]
    pub probe_enabled: bool,
    #[serde(default)]
    pub models: Vec<ModelRef>,
}

/// Why a model is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Deactivation {
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    pub set_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProviderDeactivations {
    pub provider: String,
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, Deactivation>,
}

/// Last probe of a local runtime.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProbeStatus {
    pub at: String,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub served: Vec<String>,
}

/// `GET /eavs/maintenance/availability`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Availability {
    #[serde(default)]
    pub availability: Vec<ProviderDeactivations>,
    #[serde(default)]
    pub probe: std::collections::BTreeMap<String, ProbeStatus>,
    #[serde(default)]
    pub providers: Vec<ProviderModels>,
}

impl Availability {
    /// The deactivation of `model` on `provider`, if any.
    pub fn deactivation(&self, provider: &str, model: &str) -> Option<&Deactivation> {
        self.availability
            .iter()
            .find(|p| p.provider == provider)
            .and_then(|p| p.models.get(model))
    }
}

/// One usage meter (`GET /admin/usage/subscriptions`).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Meter {
    pub id: String,
    pub label: String,
    pub unit: String,
    #[serde(default)]
    pub used: Option<f64>,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub remaining: Option<f64>,
    #[serde(default)]
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub period_secs: Option<u64>,
}

/// Usage of one upstream account.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AccountUsage {
    pub id: String,
    pub vendor: String,
    pub provider: String,
    pub account: String,
    #[serde(default)]
    pub plan: Option<String>,
    pub status: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub observed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub meters: Vec<Meter>,
}

/// One stage of a provider test.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProbeStage {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub detail: String,
}

/// `POST /admin/providers/probe` result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProbeResult {
    pub ok: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub stages: Vec<ProbeStage>,
}

/// New provider declaration (`POST /admin/providers/create`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewProvider {
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    /// Empty for keyless local runtimes.
    pub api_key: String,
    pub models: Vec<String>,
    /// Watch availability with a non-generating `GET /models` probe.
    pub probe: bool,
}

impl NewProvider {
    /// Validate before sending; errors are shown next to the form.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty()
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err("Name: lowercase letters, digits, - or _".into());
        }
        if self.provider_type.is_empty() {
            return Err("Choose a provider type".into());
        }
        if !self.base_url.is_empty()
            && !(self.base_url.starts_with("https://") || self.base_url.starts_with("http://"))
        {
            return Err("Base URL must start with https:// or http://".into());
        }
        if self.api_key.starts_with("env:") || self.api_key.starts_with("keychain:") {
            return Err("Paste the key itself; references are resolved on the gateway's machine only from its config file".into());
        }
        Ok(())
    }

    fn body(&self) -> Value {
        let mut config = json!({
            "type": self.provider_type,
            "base_url": self.base_url,
            "api_key": if self.api_key.is_empty() { "none" } else { self.api_key.as_str() },
        });
        if !self.models.is_empty() {
            config["models"] = self
                .models
                .iter()
                .map(|id| json!({"id": id, "name": id}))
                .collect();
        }
        if self.probe {
            config["probe"] = json!({"enabled": true});
        }
        json!({"name": self.name, "config": config, "enabled": true})
    }
}

/// A connected instance.
pub struct Client {
    base: String,
    token: Option<String>,
    master_key: Option<String>,
    agent: ureq::Agent,
}

impl Client {
    /// Resolve the instance's credentials and build a client.
    pub fn connect(instance: &Instance) -> Result<Self, ApiError> {
        crate::instances::validate_url(&instance.url)
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        let resolve = |reference: &Option<crate::instances::SecretRef>| {
            reference
                .as_ref()
                .map(|r| {
                    r.resolve()
                        .map_err(|e| ApiError::Unauthorized(e.to_string()))
                })
                .transpose()
        };
        Ok(Self::with_credentials(
            &instance.url,
            resolve(&instance.token)?,
            resolve(&instance.master_key)?,
        ))
    }

    /// Build a client from already-resolved credentials (tests, local).
    pub fn with_credentials(base: &str, token: Option<String>, master_key: Option<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .build();
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token,
            master_key,
            agent: config.into(),
        }
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        let url = format!("{}{path}", self.base);
        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(&url)
            .header("accept", "application/json");
        if let Some(token) = &self.token {
            builder = builder.header("x-eavs-token", token);
        }
        if let Some(master) = &self.master_key {
            builder = builder.header("authorization", format!("Bearer {master}"));
        }
        let response = match body {
            Some(body) => {
                let request = builder
                    .header("content-type", "application/json")
                    .body(body.to_string())
                    .map_err(|e| ApiError::Protocol(e.to_string()))?;
                self.agent.run(request)
            }
            None => {
                let request = builder
                    .body(())
                    .map_err(|e| ApiError::Protocol(e.to_string()))?;
                self.agent.run(request)
            }
        }
        .map_err(|e| ApiError::Unreachable(transport_message(&e)))?;
        let status = response.status().as_u16();
        let text = response
            .into_body()
            .with_config()
            .limit(MAX_BODY)
            .read_to_string()
            .map_err(|_| ApiError::Protocol("response unreadable or too large".into()))?;
        let value: Value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        classify(status, value)
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let value = self.request("GET", path, None)?;
        serde_json::from_value(value)
            .map_err(|e| ApiError::Protocol(format!("unexpected response: {e}")))
    }

    /// Provider inventory with source and revision.
    pub fn sources(&self) -> Result<Vec<ProviderSource>, ApiError> {
        self.get("/admin/providers/sources")
    }

    /// Provider universe, deactivations and probe results.
    pub fn availability(&self) -> Result<Availability, ApiError> {
        self.get("/eavs/maintenance/availability")
    }

    /// Subscription and credit usage; `refresh` asks the gateway to read due accounts now.
    pub fn usage(&self, refresh: bool) -> Result<Vec<AccountUsage>, ApiError> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            accounts: Vec<AccountUsage>,
        }
        let path = if refresh {
            "/admin/usage/subscriptions?refresh=true"
        } else {
            "/admin/usage/subscriptions"
        };
        Ok(self.get::<Envelope>(path)?.accounts)
    }

    pub fn create_provider(&self, provider: &NewProvider) -> Result<(), ApiError> {
        provider.validate().map_err(|message| ApiError::Rejected {
            status: 400,
            message,
        })?;
        self.request("POST", "/admin/providers/create", Some(&provider.body()))
            .map(|_| ())
    }

    pub fn delete_provider(&self, name: &str) -> Result<(), ApiError> {
        self.request(
            "DELETE",
            &format!("/admin/providers/{}", encode(name)),
            None,
        )
        .map(|_| ())
    }

    pub fn add_model(&self, provider: &str, id: &str) -> Result<(), ApiError> {
        self.request(
            "POST",
            &format!("/admin/providers/{}/models", encode(provider)),
            Some(&json!({"id": id, "name": id})),
        )
        .map(|_| ())
    }

    pub fn remove_model(&self, provider: &str, id: &str) -> Result<(), ApiError> {
        self.request(
            "DELETE",
            &format!(
                "/admin/providers/{}/models/{}",
                encode(provider),
                encode(id)
            ),
            None,
        )
        .map(|_| ())
    }

    pub fn rotate_key(&self, provider: &str, revision: &str, key: &str) -> Result<(), ApiError> {
        self.request(
            "POST",
            &format!("/admin/providers/{}/credentials", encode(provider)),
            Some(&json!({"expected_revision": revision, "api_key": key})),
        )
        .map(|_| ())
    }

    pub fn set_available(
        &self,
        provider: &str,
        model: &str,
        available: bool,
    ) -> Result<(), ApiError> {
        let mut body = json!({"provider": provider, "models": [model], "available": available});
        if !available {
            body["reason"] = json!("disabled from eavs console");
        }
        self.request("POST", "/eavs/maintenance/availability", Some(&body))
            .map(|_| ())
    }

    /// Test a saved provider with one small request (this generates).
    pub fn test_provider(&self, provider: &str, model: &str) -> Result<ProbeResult, ApiError> {
        let value = self.request(
            "POST",
            "/admin/providers/probe",
            Some(&json!({"provider_name": provider, "saved": true, "model": model})),
        )?;
        serde_json::from_value(value)
            .map_err(|e| ApiError::Protocol(format!("unexpected response: {e}")))
    }
}

fn encode(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

fn transport_message(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Timeout(_) => "the gateway did not answer in time".into(),
        ureq::Error::ConnectionFailed | ureq::Error::Io(_) => {
            "cannot reach the gateway (is it running, or the tunnel open?)".into()
        }
        ureq::Error::HostNotFound => "unknown host".into(),
        _ => "request failed".into(),
    }
}

/// Map a status and body to a result; the gateway's own error text is used
/// for 4xx (it is written for operators and never contains credentials).
fn classify(status: u16, body: Value) -> Result<Value, ApiError> {
    if (200..300).contains(&status) {
        return Ok(body);
    }
    let message = body
        .get("error")
        .and_then(|e| {
            e.as_str()
                .map(str::to_owned)
                .or_else(|| e.get("message").and_then(Value::as_str).map(str::to_owned))
        })
        .unwrap_or_else(|| format!("the gateway answered {status}"));
    match status {
        401 => Err(ApiError::Unauthorized(format!(
            "credentials rejected ({message}); check the listener token and master key"
        ))),
        403 => Err(ApiError::Unauthorized(message)),
        500..=599 => Err(ApiError::Rejected {
            status,
            message: format!("gateway error: {message}"),
        }),
        _ => Err(ApiError::Rejected { status, message }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_gateway_errors() {
        assert!(classify(200, json!({"ok": true})).is_ok());
        assert_eq!(
            classify(
                409,
                json!({"error": "Provider 'x' already exists", "code": "conflict"})
            ),
            Err(ApiError::Rejected {
                status: 409,
                message: "Provider 'x' already exists".into()
            })
        );
        assert!(matches!(
            classify(401, json!({"error": "Listener authentication required"})),
            Err(ApiError::Unauthorized(_))
        ));
        assert!(matches!(
            classify(502, Value::Null),
            Err(ApiError::Rejected { status: 502, .. })
        ));
    }

    #[test]
    fn new_provider_body_and_validation() {
        let provider = NewProvider {
            name: "rtx".into(),
            provider_type: "openai-compatible".into(),
            base_url: "http://127.0.0.1:8080/v1".into(),
            api_key: String::new(),
            models: vec!["qwen3.8".into()],
            probe: true,
        };
        assert!(provider.validate().is_ok());
        let body = provider.body();
        assert_eq!(body["config"]["api_key"], "none");
        assert_eq!(body["config"]["models"][0]["id"], "qwen3.8");
        assert_eq!(body["config"]["probe"]["enabled"], true);
        let bad = NewProvider {
            name: "Bad Name".into(),
            ..provider.clone()
        };
        assert!(bad.validate().is_err());
        let reference = NewProvider {
            api_key: "keychain:x".into(),
            ..provider
        };
        assert!(reference.validate().is_err());
    }

    #[test]
    fn availability_lookup() {
        let snapshot: Availability = serde_json::from_value(json!({
            "availability": [{"provider": "rtx", "models": {"kimi": {"reason": "loadout", "set_by": "probe"}}}],
            "probe": {"rtx": {"at": "2026-10-27T10:00:00Z", "ok": true, "served": ["qwen"]}},
            "providers": [{"provider": "rtx", "type": "openai-compatible", "probe_enabled": true,
                           "models": [{"id": "kimi", "name": "Kimi"}, {"id": "qwen", "name": ""}]}]
        }))
        .unwrap();
        assert_eq!(
            snapshot.deactivation("rtx", "kimi").unwrap().set_by,
            "probe"
        );
        assert!(snapshot.deactivation("rtx", "qwen").is_none());
    }

    #[test]
    fn unreachable_instance_is_classified() {
        // Port 9 on loopback (discard) is closed in test environments.
        let client = Client::with_credentials("http://127.0.0.1:9", None, None);
        assert!(matches!(client.sources(), Err(ApiError::Unreachable(_))));
    }
}
