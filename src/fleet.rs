//! Shared eavs fleet client and CLI operations.
//!
//! Manages a fleet of remote eavs instances declared in a TOML registry. Each
//! instance references an admin API key stored in an environment variable. The
//! module is deliberately self-contained: it depends only on external crates and
//! is meant to be consumed by both the CLI (`FleetArgs`/`run_cli`) and the parent
//! TUI through the small public facade below:
//!
//! ```text
//! FleetConfig { pub instances: Vec<Instance> }
//! Instance (Clone) { pub name, pub url, pub admin_key_env: String }
//! load_config(path) -> anyhow::Result<FleetConfig>
//! ProviderSource (Clone + Deserialize) { name, provider_type, source, revision, enabled }
//! inventory(instance) -> anyhow::Result<Vec<ProviderSource>>
//! Change (Clone) { Add{name,provider_type,base_url,key_env} | Rotate{name,revision,key_env} | Rollback{name,revision} }
//! apply(instance, change) -> anyhow::Result<()>
//! FleetArgs (derive clap::Args)
//! run_cli(args) -> anyhow::Result<()>
//! ```
//!
//! # Transport contract
//!
//! * Instance URLs must be HTTPS, except literal loopback hosts may use plain
//!   HTTP. Userinfo, query and fragments are rejected, as are redirects.
//! * Requests carry bounded connect/read timeouts and a bounded response size.
//! * Errors never include the upstream response body, and never include the
//!   resolved secret values or a secret-bearing URL.
//! * Admin authentication is resolved from `instance.admin_key_env` only when an
//!   operation (`inventory` or `apply`) actually runs.
//!
//! # Mutation contract
//!
//! * Mutations are never retried automatically. `add` refuses a name already
//!   present in the inventory and uses the create-only endpoint
//!   `POST /admin/providers/create`. `rotate`/`rollback` use the exact revision
//!   pinned on the change (a conflict is reported, not silently re-fetched).

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::{Host, Url};

/// Bound for a single response body in bytes (1 MiB).
const MAX_RESPONSE_BYTES: usize = 1 << 20;
/// Total per-request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Registry types
// ---------------------------------------------------------------------------

/// Fleet TOML registry.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FleetConfig {
    /// Declared eavs instances.
    pub instances: Vec<Instance>,
}

/// A single remote eavs instance.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    /// Unique instance name.
    pub name: String,
    /// Base URL of the eavs control plane.
    pub url: String,
    /// Name of the environment variable holding the admin API key.
    pub admin_key_env: String,
}

/// A provider source as reported by `GET /admin/providers/sources`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProviderSource {
    /// Provider name.
    pub name: String,
    /// Provider type (e.g. `openai-compatible`).
    pub provider_type: String,
    /// Origin of the declaration (e.g. `admin` or `toml`).
    pub source: String,
    /// Provider revision used for CAS operations, when applicable.
    pub revision: Option<String>,
    /// Whether the provider is enabled.
    pub enabled: bool,
}

/// Request body for `POST /admin/providers/create` (atomic create only).
///
/// Deliberately does not implement `Debug`: `config` may carry a resolved API
/// key that must never appear in logs or error output.
#[derive(Clone, Serialize)]
pub struct CreateProviderRequest {
    /// Provider name.
    pub name: String,
    /// Provider configuration.
    pub config: serde_json::Value,
    /// Whether the provider should start enabled.
    pub enabled: bool,
}

/// Request body for `POST /admin/providers/:name/credentials` (rotate).
///
/// Deliberately does not implement `Debug`: `api_key` is a resolved secret.
#[derive(Clone, Serialize)]
pub struct RotateRequest {
    /// Expected provider revision (CAS).
    pub expected_revision: String,
    /// New upstream API key.
    pub api_key: String,
}

/// Request body for `POST /admin/providers/:name/credentials/rollback`.
#[derive(Debug, Clone, Serialize)]
pub struct RollbackRequest {
    /// Expected provider revision (CAS).
    pub expected_revision: String,
}

/// A planned fleet change. Carries only env-var *names*, never resolved secrets.
#[derive(Debug, Clone)]
pub enum Change {
    /// Create a new provider (create-only; name must not already exist).
    Add {
        /// Provider name.
        name: String,
        /// Provider type.
        provider_type: String,
        /// Upstream base URL.
        base_url: String,
        /// Env var holding the upstream API key (empty allowed only for
        /// loopback unauthenticated upstreams).
        key_env: String,
    },
    /// Rotate an existing provider's upstream API key (CAS on `revision`).
    Rotate {
        /// Provider name.
        name: String,
        /// Revision to rotate against.
        revision: String,
        /// Env var holding the new upstream API key.
        key_env: String,
    },
    /// Roll back an existing provider's upstream credentials (CAS on `revision`).
    Rollback {
        /// Provider name.
        name: String,
        /// Revision to roll back against.
        revision: String,
    },
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

impl Change {
    pub fn provider_name(&self) -> &str {
        match self {
            Self::Add { name, .. } | Self::Rotate { name, .. } | Self::Rollback { name, .. } => {
                name
            }
        }
    }
}

/// Load, parse and validate the fleet TOML registry at `path`.
///
/// Validation runs before any secret environment variable is read: it rejects
/// empty registries, duplicate/invalid instance names, malformed URLs and
/// invalid admin env var syntax. The TOML parser diagnostic is deliberately
/// withheld (it can echo secret-bearing source lines) and the URL referred to
/// is never echoed back into an error.
pub fn load_config(path: &Path) -> anyhow::Result<FleetConfig> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read fleet config '{}': {e}", path.display()))?;
    let config: FleetConfig = toml::from_str(&text)
        .map_err(|_| anyhow::anyhow!("invalid fleet config '{}'", path.display()))?;
    validate_registry(&config)?;
    Ok(config)
}

/// Validate the whole registry: non-empty, unique valid instance names, valid
/// URLs and valid admin env var names. Does not read any secret value.
fn validate_registry(config: &FleetConfig) -> anyhow::Result<()> {
    if config.instances.is_empty() {
        return Err(anyhow::anyhow!(
            "fleet config must declare at least one instance"
        ));
    }
    let mut seen = HashSet::new();
    for instance in &config.instances {
        validate_instance(instance)?;
        if !seen.insert(&instance.name) {
            return Err(anyhow::anyhow!(
                "duplicate fleet instance name '{}'",
                instance.name
            ));
        }
    }
    Ok(())
}

/// Validate a single instance before any secret is read.
fn validate_instance(instance: &Instance) -> anyhow::Result<()> {
    if instance.name.is_empty() || instance.name.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(anyhow::anyhow!(
            "fleet instance has an invalid 'name' (must be non-empty without control characters)"
        ));
    }
    if instance.url.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(anyhow::anyhow!(
            "fleet instance '{}' has an invalid 'url'",
            instance.name
        ));
    }
    validate_admin_url(&instance.url)?;
    validate_env_name(&instance.admin_key_env)
}

// ---------------------------------------------------------------------------
// Transport helpers
// ---------------------------------------------------------------------------

/// Whether a host is a loopback address, treating the conventional
/// `localhost` name as loopback but otherwise relying on the IP class
/// (`IPv4` 127.0.0.0/8, `IPv6` `::1`) rather than trusting DNS resolution.
fn is_loopback_host<T: AsRef<str>>(host: &Host<T>) -> bool {
    match host {
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
        Host::Domain(domain) => domain.as_ref().eq_ignore_ascii_case("localhost"),
    }
}

fn is_loopback_url(raw: &str) -> bool {
    Url::parse(raw).is_ok_and(|url| url.host().is_some_and(|host| is_loopback_host(&host)))
}

/// Validate an instance/admin URL per the transport contract.
fn validate_admin_url(raw: &str) -> anyhow::Result<Url> {
    // Do not echo the raw URL or the parser diagnostic; either may carry a
    // secret (e.g. userinfo) or secret-bearing source text.
    let url = Url::parse(raw).map_err(|_| anyhow::anyhow!("invalid fleet URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(anyhow::anyhow!("fleet URL must not contain userinfo"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(anyhow::anyhow!(
            "fleet URL must not contain a query or fragment"
        ));
    }
    match url.scheme() {
        "https" => {}
        "http" => {
            let host = url
                .host()
                .ok_or_else(|| anyhow::anyhow!("fleet URL has no host"))?;
            if !is_loopback_host(&host) {
                return Err(anyhow::anyhow!(
                    "fleet URL must use https (plain http is allowed only for loopback hosts)"
                ));
            }
        }
        scheme => {
            return Err(anyhow::anyhow!(
                "fleet URL scheme '{scheme}' is not allowed"
            ));
        }
    }
    Ok(url)
}

fn build_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()?)
}

fn client_for(instance: &Instance) -> anyhow::Result<(Url, reqwest::Client)> {
    let mut base = validate_admin_url(&instance.url)?;
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    Ok((base, build_client()?))
}

/// Validate that an environment variable name is non-empty and well-formed.
fn validate_env_name(var: &str) -> anyhow::Result<()> {
    if var.is_empty() {
        return Err(anyhow::anyhow!(
            "environment variable name must not be empty"
        ));
    }
    let valid = var
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if !valid || var.as_bytes()[0].is_ascii_digit() {
        return Err(anyhow::anyhow!(
            "environment variable reference must be an identifier, not a literal secret"
        ));
    }
    Ok(())
}

/// Validate a provider name is a safe, single URL path segment.
fn validate_provider_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        return Err(anyhow::anyhow!("provider name must not be empty"));
    }
    let segment_safe = name.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
    });
    if !segment_safe {
        return Err(anyhow::anyhow!(
            "provider name '{name}' contains invalid characters; only lowercase \
             letters, digits, hyphens and underscores are allowed"
        ));
    }
    Ok(())
}

fn resolve_env(var: &str, context: &str) -> anyhow::Result<String> {
    validate_env_name(var)?;
    let value = std::env::var(var)
        .map_err(|_| anyhow::anyhow!("environment variable '{var}' for {context} is not set"))?;
    if value.trim().is_empty()
        || value.len() > 16_384
        || value.chars().any(char::is_control)
        || value.starts_with("env:")
        || value.starts_with("keychain:")
    {
        return Err(anyhow::anyhow!("Secret reference must resolve to a non-empty literal credential, not another reference"));
    }
    Ok(value)
}

async fn read_bounded(mut resp: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    if let Some(len) = resp.content_length() {
        if len > MAX_RESPONSE_BYTES as u64 {
            return Err(anyhow::anyhow!(
                "fleet response exceeds {MAX_RESPONSE_BYTES} bytes"
            ));
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("failed to read fleet response"))?
    {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(anyhow::anyhow!(
                "fleet response exceeds {MAX_RESPONSE_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Build a sanitized error from a non-success response. Never includes the
/// upstream response body or secret-bearing content — only the error code.
fn error_from_body(status: reqwest::StatusCode, _bytes: &[u8]) -> anyhow::Error {
    // Even a field named `code` is untrusted response content and could contain
    // secrets or terminal controls. Emit only locally-defined classifications.
    let classification = match status.as_u16() {
        409 => "revision_conflict or provider already exists; refresh and review",
        401 | 403 => "admin authentication denied",
        400 => "invalid provider operation",
        404 => "provider or fleet API unavailable; check server version",
        _ => "server rejected operation",
    };
    anyhow::anyhow!("fleet request failed ({status}): {classification}")
}

/// Perform a single authenticated request, enforcing redirect rejection,
/// timeouts, response bounds and sanitized errors.
async fn request(
    client: &reqwest::Client,
    base: &Url,
    path: &str,
    auth: &str,
    method: reqwest::Method,
    body: Option<&serde_json::Value>,
) -> anyhow::Result<Vec<u8>> {
    let url = base
        .join(path.trim_start_matches('/'))
        .map_err(|e| anyhow::anyhow!("invalid fleet request URL: {e}"))?;
    let mut req = client
        .request(method, url)
        .header("authorization", format!("Bearer {auth}"));
    if let Some(payload) = body {
        req = req.json(payload);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("fleet request failed: {}", e.without_url()))?;
    let status = resp.status();
    if status.is_redirection() {
        return Err(anyhow::anyhow!(
            "fleet returned a redirect status ({status}); redirects are not allowed"
        ));
    }
    let bytes = read_bounded(resp).await?;
    if !status.is_success() {
        return Err(error_from_body(status, &bytes));
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Public facade operations
// ---------------------------------------------------------------------------

/// Fetch the provider source inventory for `instance`.
///
/// Resolves the admin API key environment variable only here (an explicit
/// operation) and issues `GET /admin/providers/sources`.
pub async fn inventory(instance: &Instance) -> anyhow::Result<Vec<ProviderSource>> {
    let (base, client) = client_for(instance)?;
    let auth = resolve_env(
        &instance.admin_key_env,
        &format!("fleet instance '{}'", instance.name),
    )?;
    let bytes = request(
        &client,
        &base,
        "/admin/providers/sources",
        &auth,
        reqwest::Method::GET,
        None,
    )
    .await?;
    let sources: Vec<ProviderSource> = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("fleet returned an invalid source inventory"))?;
    Ok(sources)
}

/// Apply a planned change to `instance`.
///
/// Resolves environment secrets only here. Mutations are never retried
/// automatically; `add` refuses a name already in the inventory and uses the
/// create-only endpoint, while `rotate`/`rollback` use the pinned revision.
pub async fn apply(instance: &Instance, change: &Change) -> anyhow::Result<()> {
    validate_change(change)?;
    let (base, client) = client_for(instance)?;
    let auth = resolve_env(
        &instance.admin_key_env,
        &format!("fleet instance '{}'", instance.name),
    )?;
    match change {
        Change::Add {
            name,
            provider_type,
            base_url,
            key_env,
        } => {
            let inventory = fetch_sources(&client, &base, &auth).await?;
            if inventory.iter().any(|entry| entry.name == *name) {
                return Err(anyhow::anyhow!(
                    "provider '{name}' already exists on fleet instance '{}'",
                    instance.name
                ));
            }
            let api_key = if key_env.trim().is_empty() {
                if !is_loopback_url(base_url) {
                    return Err(anyhow::anyhow!(
                        "provider '{name}' add requires a non-empty --key-env; an \
                         unauthenticated upstream is only allowed for a loopback base URL"
                    ));
                }
                String::new()
            } else {
                resolve_env(key_env, &format!("provider '{name}' API key"))?
            };
            let config = serde_json::json!({
                "type": provider_type,
                "base_url": base_url,
                "api_key": api_key,
            });
            let body = serde_json::to_value(&CreateProviderRequest {
                name: name.clone(),
                config,
                enabled: true,
            })?;
            request(
                &client,
                &base,
                "/admin/providers/create",
                &auth,
                reqwest::Method::POST,
                Some(&body),
            )
            .await?;
            Ok(())
        }
        Change::Rotate {
            name,
            revision,
            key_env,
        } => {
            if key_env.trim().is_empty() {
                return Err(anyhow::anyhow!(
                    "provider '{name}' rotate requires a non-empty --key-env"
                ));
            }
            let api_key = resolve_env(key_env, &format!("provider '{name}' API key"))?;
            let body = serde_json::to_value(&RotateRequest {
                expected_revision: revision.clone(),
                api_key,
            })?;
            request(
                &client,
                &base,
                &format!("/admin/providers/{name}/credentials"),
                &auth,
                reqwest::Method::POST,
                Some(&body),
            )
            .await?;
            Ok(())
        }
        Change::Rollback { name, revision } => {
            let body = serde_json::to_value(&RollbackRequest {
                expected_revision: revision.clone(),
            })?;
            request(
                &client,
                &base,
                &format!("/admin/providers/{name}/credentials/rollback"),
                &auth,
                reqwest::Method::POST,
                Some(&body),
            )
            .await?;
            Ok(())
        }
    }
}

async fn fetch_sources(
    client: &reqwest::Client,
    base: &Url,
    auth: &str,
) -> anyhow::Result<Vec<ProviderSource>> {
    let bytes = request(
        client,
        base,
        "/admin/providers/sources",
        auth,
        reqwest::Method::GET,
        None,
    )
    .await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("fleet returned an invalid source inventory"))
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// Fleet root arguments, wired by the parent as an `eavs fleet` subcommand.
#[derive(Debug, clap::Args)]
pub struct FleetArgs {
    /// Fleet registry TOML (defaults to XDG eavs/fleet.toml).
    #[arg(long, env = "EAVS_FLEET")]
    pub fleet: Option<std::path::PathBuf>,

    /// Fleet operation to run.
    #[command(subcommand)]
    pub command: FleetCommand,
}

/// Fleet subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum FleetCommand {
    /// Print a secret-free registry template to stdout (does not write files)
    Init,
    /// List provider sources across fleet instances.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Add a provider to fleet instances (create-only).
    Add {
        /// Explicit target instances; repeat to select multiple machines.
        #[arg(long, required = true, action = clap::ArgAction::Append)]
        instance: Vec<String>,
        /// Provider name.
        provider: String,
        /// Env var holding the upstream API key.
        #[arg(long)]
        key_env: String,
        /// Provider type.
        #[arg(long)]
        provider_type: String,
        /// Upstream base URL.
        #[arg(long)]
        base_url: String,
        /// Actually apply instead of printing a preview.
        #[arg(long)]
        yes: bool,
    },
    /// Rotate a provider's upstream API key.
    Rotate {
        /// Explicit target instances; repeat to select multiple machines.
        #[arg(long, required = true, action = clap::ArgAction::Append)]
        instance: Vec<String>,
        /// Provider name.
        provider: String,
        /// Env var holding the new upstream API key.
        #[arg(long)]
        key_env: String,
        /// Actually apply instead of printing a preview.
        #[arg(long)]
        yes: bool,
    },
    /// Roll back a provider's upstream credentials.
    Rollback {
        /// Explicit target instances; repeat to select multiple machines.
        #[arg(long, required = true, action = clap::ArgAction::Append)]
        instance: Vec<String>,
        /// Provider name.
        provider: String,
        /// Actually apply instead of printing a preview.
        #[arg(long)]
        yes: bool,
    },
}

/// Machine-readable list result.
#[derive(Debug, Serialize)]
struct ListOutput {
    /// Per-instance inventory results.
    instances: Vec<ListInstance>,
}

/// Per-instance list entry (sources plus any error).
#[derive(Debug, Serialize)]
struct ListInstance {
    /// Instance name.
    name: String,
    /// Sources, empty when the instance failed.
    sources: Vec<ProviderSource>,
    /// Sanitized error message, when the instance failed.
    error: Option<String>,
}

fn select_instances<'a>(
    config: &'a FleetConfig,
    names: &[String],
) -> anyhow::Result<Vec<&'a Instance>> {
    if names.is_empty() {
        return Ok(config.instances.iter().collect());
    }
    let mut selected = Vec::with_capacity(names.len());
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(anyhow::anyhow!("Target instance listed more than once"));
        }
        let instance = config
            .instances
            .iter()
            .find(|entry| &entry.name == name)
            .ok_or_else(|| anyhow::anyhow!("unknown fleet instance '{name}'"))?;
        selected.push(instance);
    }
    Ok(selected)
}

#[derive(Clone)]
struct Failure {
    instance: String,
    error: String,
}

fn report_failures(failures: &[Failure]) -> anyhow::Result<()> {
    if failures.is_empty() {
        return Ok(());
    }
    let details = failures
        .iter()
        .map(|failure| format!("{}: {}", failure.instance, failure.error))
        .collect::<Vec<_>>()
        .join("; ");
    Err(anyhow::anyhow!(
        "{} fleet operation(s) failed: {details}",
        failures.len()
    ))
}

/// Run a fleet CLI command.
pub async fn run_cli(args: FleetArgs) -> anyhow::Result<()> {
    use crate::program_status::{Reporter, State};
    use std::io::IsTerminal as _;
    let mutating = matches!(
        &args.command,
        FleetCommand::Add { yes: true, .. }
            | FleetCommand::Rotate { yes: true, .. }
            | FleetCommand::Rollback { yes: true, .. }
    );
    let terminal = std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal();
    let mut reporter = Reporter::new(
        std::io::stderr(),
        crate::program_status::enabled_for_terminal(terminal && mutating),
    );
    reporter.set(
        State::Working,
        "Applying explicitly selected fleet operations",
    );
    let result = run_cli_inner(args).await;
    reporter.set(
        if result.is_ok() {
            State::Done
        } else {
            State::Error
        },
        "Fleet command settled; inspect per-instance results",
    );
    result
}

async fn run_cli_inner(args: FleetArgs) -> anyhow::Result<()> {
    if matches!(args.command, FleetCommand::Init) {
        print!("{}", include_str!("../examples/fleet.toml"));
        return Ok(());
    }
    let path = match &args.fleet {
        Some(path) => path.clone(),
        None => crate::paths::config_dir()?.join("eavs").join("fleet.toml"),
    };
    let config = load_config(&path)?;
    match &args.command {
        FleetCommand::Init => Ok(()),
        FleetCommand::List { json } => run_list(&config, *json).await,
        FleetCommand::Add {
            instance,
            provider,
            key_env,
            provider_type,
            base_url,
            yes,
        } => {
            run_add(
                &config,
                instance,
                provider,
                key_env,
                provider_type,
                base_url,
                *yes,
            )
            .await
        }
        FleetCommand::Rotate {
            instance,
            provider,
            key_env,
            yes,
        } => run_rotate(&config, instance, provider, key_env, *yes).await,
        FleetCommand::Rollback {
            instance,
            provider,
            yes,
        } => run_rollback(&config, instance, provider, *yes).await,
    }
}

async fn run_list(config: &FleetConfig, json: bool) -> anyhow::Result<()> {
    let mut outputs = Vec::new();
    let mut failures = Vec::new();
    for instance in &config.instances {
        match inventory(instance).await {
            Ok(sources) => {
                if json {
                    outputs.push(ListInstance {
                        name: instance.name.clone(),
                        sources,
                        error: None,
                    });
                } else {
                    println!("[{}]", instance.name);
                    for source in &sources {
                        println!(
                            "  {}\ttype={}\tsource={}\trevision={}\tenabled={}",
                            source.name,
                            source.provider_type,
                            source.source,
                            source.revision.as_deref().unwrap_or("none"),
                            source.enabled,
                        );
                    }
                }
            }
            Err(error) => {
                let message = error.to_string();
                if json {
                    outputs.push(ListInstance {
                        name: instance.name.clone(),
                        sources: Vec::new(),
                        error: Some(message.clone()),
                    });
                } else {
                    eprintln!("[{}] error: {message}", instance.name);
                }
                failures.push(Failure {
                    instance: instance.name.clone(),
                    error: message,
                });
            }
        }
    }
    if json {
        let output = ListOutput { instances: outputs };
        println!("{}", serde_json::to_string_pretty(&output)?);
    }
    report_failures(&failures)
}

/// Validate a plan without reading credentials or contacting an instance.
pub fn validate_change(change: &Change) -> anyhow::Result<()> {
    validate_provider_name(change.provider_name())?;
    match change {
        Change::Add {
            provider_type,
            base_url,
            key_env,
            ..
        } => {
            if crate::provider::ProviderType::try_from_str(provider_type).is_none() {
                return Err(anyhow::anyhow!("Unknown provider type"));
            }
            validate_admin_url(base_url)?;
            if !key_env.is_empty() {
                validate_env_name(key_env)?;
            }
        }
        Change::Rotate { key_env, .. } => validate_env_name(key_env)?,
        Change::Rollback { .. } => {}
    }
    Ok(())
}

fn describe_change(change: &Change) -> String {
    match change {
        Change::Add {
            name,
            provider_type,
            base_url,
            key_env,
        } => format!(
            "add provider '{name}' (type={provider_type}, base_url={base_url}, key_env={key_env})"
        ),
        Change::Rotate {
            name,
            revision,
            key_env,
        } => format!("rotate provider '{name}' (revision={revision}, key_env={key_env})"),
        Change::Rollback { name, revision } => {
            format!("rollback provider '{name}' (revision={revision})")
        }
    }
}

async fn preview_and_apply(
    instance: &Instance,
    change: &Change,
    yes: bool,
    verb: &str,
) -> anyhow::Result<()> {
    validate_change(change)?;
    println!("[{}] preview: {}", instance.name, describe_change(change));
    if yes {
        apply(instance, change).await?;
        println!("[{}] applied {verb}", instance.name);
    } else {
        println!("[{}] dry-run (pass --yes to apply)", instance.name);
    }
    Ok(())
}

async fn run_add(
    config: &FleetConfig,
    names: &[String],
    provider: &str,
    key_env: &str,
    provider_type: &str,
    base_url: &str,
    yes: bool,
) -> anyhow::Result<()> {
    let instances = select_instances(config, names)?;
    let mut failures = Vec::new();
    for instance in instances {
        let change = Change::Add {
            name: provider.to_string(),
            provider_type: provider_type.to_string(),
            base_url: base_url.to_string(),
            key_env: key_env.to_string(),
        };
        if let Err(error) = preview_and_apply(instance, &change, yes, "add").await {
            failures.push(Failure {
                instance: instance.name.clone(),
                error: error.to_string(),
            });
        }
    }
    report_failures(&failures)
}

async fn run_rotate(
    config: &FleetConfig,
    names: &[String],
    provider: &str,
    key_env: &str,
    yes: bool,
) -> anyhow::Result<()> {
    let instances = select_instances(config, names)?;
    let mut failures = Vec::new();
    for instance in instances {
        match inventory(instance).await {
            Ok(sources) => {
                let Some(revision) = sources
                    .iter()
                    .find(|entry| entry.name == provider && entry.source == "admin")
                    .and_then(|entry| entry.revision.clone())
                else {
                    failures.push(Failure {
                        instance: instance.name.clone(),
                        error: "Provider is missing, TOML-managed or has no revision".into(),
                    });
                    continue;
                };
                let change = Change::Rotate {
                    name: provider.to_string(),
                    revision,
                    key_env: key_env.to_string(),
                };
                if let Err(error) = preview_and_apply(instance, &change, yes, "rotate").await {
                    failures.push(Failure {
                        instance: instance.name.clone(),
                        error: error.to_string(),
                    });
                }
            }
            Err(error) => failures.push(Failure {
                instance: instance.name.clone(),
                error: error.to_string(),
            }),
        }
    }
    report_failures(&failures)
}

async fn run_rollback(
    config: &FleetConfig,
    names: &[String],
    provider: &str,
    yes: bool,
) -> anyhow::Result<()> {
    let instances = select_instances(config, names)?;
    let mut failures = Vec::new();
    for instance in instances {
        match inventory(instance).await {
            Ok(sources) => {
                let Some(revision) = sources
                    .iter()
                    .find(|entry| entry.name == provider && entry.source == "admin")
                    .and_then(|entry| entry.revision.clone())
                else {
                    failures.push(Failure {
                        instance: instance.name.clone(),
                        error: "Provider is missing, TOML-managed or has no revision".into(),
                    });
                    continue;
                };
                let change = Change::Rollback {
                    name: provider.to_string(),
                    revision,
                };
                if let Err(error) = preview_and_apply(instance, &change, yes, "rollback").await {
                    failures.push(Failure {
                        instance: instance.name.clone(),
                        error: error.to_string(),
                    });
                }
            }
            Err(error) => failures.push(Failure {
                instance: instance.name.clone(),
                error: error.to_string(),
            }),
        }
    }
    report_failures(&failures)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Mutex;

    #[derive(Clone)]
    struct Route {
        method: String,
        path: String,
        status: u16,
        body: Vec<u8>,
        declared_length: Option<usize>,
        omit_length: bool,
    }

    #[derive(Clone, Default)]
    struct Recorded {
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    }

    #[derive(Clone)]
    struct MockState {
        routes: Arc<Mutex<Vec<Route>>>,
        requests: Arc<Mutex<Vec<Recorded>>>,
    }

    async fn read_mock_headers(stream: &mut TcpStream) -> Option<Vec<u8>> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 2048];
        loop {
            let count = stream.read(&mut chunk).await.ok()?;
            if count == 0 || buffer.len() > MAX_RESPONSE_BYTES {
                return None;
            }
            buffer.extend_from_slice(&chunk[..count]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                return Some(buffer);
            }
        }
    }

    async fn handle_conn(mut stream: TcpStream, state: MockState) {
        let Some(buf) = read_mock_headers(&mut stream).await else {
            return;
        };
        let mut chunk = [0u8; 2048];
        let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            return;
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split(' ');
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("/").to_string();
        let content_length: usize = lines
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse().ok())
            .unwrap_or(0);
        let mut body = buf[header_end + 4..].to_vec();
        while body.len() < content_length {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
            }
        }
        let headers = head
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        state.requests.lock().await.push(Recorded {
            method: method.clone(),
            path: path.clone(),
            headers,
            body,
        });
        let route = state
            .routes
            .lock()
            .await
            .iter()
            .find(|route| route.method == method && route.path == path)
            .cloned();
        if let Some(route) = route {
            let length = route.declared_length.unwrap_or(route.body.len());
            let head = format!(
                "HTTP/1.1 {} X\r\ncontent-type: application/json\r\n{}connection: close\r\n\r\n",
                route.status,
                if route.omit_length {
                    String::new()
                } else {
                    format!("content-length: {length}\r\n")
                }
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&route.body).await;
        } else {
            let body = br#"{"error":"not found","code":"not_found"}"#;
            let head = format!(
                    "HTTP/1.1 404 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body).await;
        }
        let _ = stream.flush().await;
    }

    async fn spawn_mock(routes: Vec<Route>) -> anyhow::Result<(String, MockState)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let url = format!("http://127.0.0.1:{}", address.port());
        let state = MockState {
            routes: Arc::new(Mutex::new(routes)),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let server_state = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let conn_state = server_state.clone();
                tokio::spawn(async move {
                    handle_conn(stream, conn_state).await;
                });
            }
        });
        Ok((url, state))
    }

    fn route(method: &str, path: &str, status: u16, body: impl Into<Vec<u8>>) -> Route {
        Route {
            method: method.to_string(),
            path: path.to_string(),
            status,
            body: body.into(),
            declared_length: None,
            omit_length: false,
        }
    }

    type SourceFixture<'a> = (&'a str, &'a str, &'a str, Option<&'a str>, bool);

    fn sources_json(entries: &[SourceFixture<'_>]) -> Vec<u8> {
        let values: Vec<serde_json::Value> = entries
            .iter()
            .map(|(name, provider_type, source, revision, enabled)| {
                serde_json::json!({
                    "name": name,
                    "provider_type": provider_type,
                    "source": source,
                    "revision": revision,
                    "enabled": enabled,
                })
            })
            .collect();
        serde_json::Value::Array(values).to_string().into_bytes()
    }

    fn hosts(url: &str) -> (String, String, String) {
        let admin = format!("{}_ADMIN", url.replace([':', '/', '.'], "_"));
        let key = format!("{}_KEY", url.replace([':', '/', '.'], "_"));
        std::env::remove_var(&admin);
        std::env::remove_var(&key);
        (admin.clone(), key.clone(), format!("{admin}_{key}"))
    }

    fn instance(url: &str, admin_env: &str) -> Instance {
        Instance {
            name: "test".to_string(),
            url: url.to_string(),
            admin_key_env: admin_env.to_string(),
        }
    }

    #[test]
    fn fleet_mutations_require_explicit_targets_and_init_needs_no_registry() {
        let missing = <crate::cli::Cli as clap::Parser>::try_parse_from([
            "eavs",
            "fleet",
            "rotate",
            "openai",
            "--key-env",
            "NEW_KEY",
            "--yes",
        ]);
        assert!(missing.is_err());
        assert!(
            <crate::cli::Cli as clap::Parser>::try_parse_from(["eavs", "fleet", "init"]).is_ok()
        );
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify reverse-proxy path preservation; URL fixture parsing propagates errors"
    )]
    fn fleet_base_path_is_preserved() -> anyhow::Result<()> {
        let (base, _) = client_for(&instance("https://example.test/gateway", "UNREAD_ENV"))?;
        assert_eq!(
            base.join("admin/providers/sources")?.path(),
            "/gateway/admin/providers/sources"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify one failed target does not abort others; fixture errors propagate"
    )]
    async fn batch_rotation_reports_missing_target_and_applies_other_targets() -> anyhow::Result<()>
    {
        let (missing_url, missing) = spawn_mock(vec![route(
            "GET",
            "/admin/providers/sources",
            200,
            sources_json(&[]),
        )])
        .await?;
        let (ready_url, ready) = spawn_mock(vec![
            route(
                "GET",
                "/admin/providers/sources",
                200,
                sources_json(&[("openai", "openai", "admin", Some("r1"), true)]),
            ),
            route(
                "POST",
                "/admin/providers/openai/credentials",
                200,
                b"{}".to_vec(),
            ),
        ])
        .await?;
        let (missing_admin, _, _) = hosts(&missing_url);
        let (ready_admin, provider_key, _) = hosts(&ready_url);
        std::env::set_var(&missing_admin, "synthetic-admin");
        std::env::set_var(&ready_admin, "synthetic-admin");
        std::env::set_var(&provider_key, "synthetic-provider");
        let mut missing_instance = instance(&missing_url, &missing_admin);
        missing_instance.name = "missing".into();
        let mut ready_instance = instance(&ready_url, &ready_admin);
        ready_instance.name = "ready".into();
        let config = FleetConfig {
            instances: vec![missing_instance, ready_instance],
        };
        assert!(run_rotate(
            &config,
            &["missing".into(), "ready".into()],
            "openai",
            &provider_key,
            true
        )
        .await
        .is_err());
        assert!(missing
            .requests
            .lock()
            .await
            .iter()
            .all(|request| request.method == "GET"));
        assert_eq!(
            ready
                .requests
                .lock()
                .await
                .iter()
                .filter(|request| request.method == "POST")
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn untrusted_error_codes_never_echo_response_secrets() {
        let error = error_from_body(
            reqwest::StatusCode::CONFLICT,
            br#"{"code":"sk-secret-response","error":"private"}"#,
        );
        assert!(error.to_string().contains("revision_conflict"));
        assert!(!error.to_string().contains("sk-secret"));
        assert!(!error.to_string().contains("private"));
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify unknown-length response bounds while fixture errors propagate"
    )]
    async fn unknown_length_response_is_bounded() -> anyhow::Result<()> {
        let mut oversized = route(
            "GET",
            "/admin/providers/sources",
            200,
            vec![b'x'; MAX_RESPONSE_BYTES + 1],
        );
        oversized.omit_length = true;
        let (url, _) = spawn_mock(vec![oversized]).await?;
        let (admin_env, _, _) = hosts(&url);
        std::env::set_var(&admin_env, "synthetic-admin");
        let error = inventory(&instance(&url, &admin_env))
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("Expected bounded response rejection"))?;
        assert!(error.to_string().contains("exceeds"));
        Ok(())
    }

    // -- URL validation -----------------------------------------------------

    #[test]
    fn url_validation_accepts_https_and_loopback_http() {
        assert!(validate_admin_url("https://eavs.example.com").is_ok());
        assert!(validate_admin_url("http://localhost:8080").is_ok());
        assert!(validate_admin_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_admin_url("http://[::1]:8080").is_ok());
    }

    #[test]
    fn url_validation_rejects_plain_http_remote_and_extras() {
        assert!(validate_admin_url("http://eavs.example.com").is_err());
        assert!(validate_admin_url("http://user:pw@127.0.0.1").is_err()); // userinfo
        assert!(validate_admin_url("https://eavs.example.com/path?q=1").is_err()); // query
        assert!(validate_admin_url("https://eavs.example.com/path#frag").is_err()); // fragment
        assert!(validate_admin_url("ftp://eavs.example.com").is_err());
        assert!(validate_admin_url("not-a-url").is_err());
    }

    #[test]
    fn env_name_validation_checks_syntax() {
        assert!(validate_env_name("EAVS_ADMIN_KEY").is_ok());
        assert!(validate_env_name("_internal").is_ok());
        assert!(validate_env_name("").is_err());
        assert!(validate_env_name("1BAD").is_err());
        assert!(validate_env_name("BAD-NAME").is_err());
        assert!(validate_env_name("BAD NAME").is_err());
    }

    #[test]
    fn provider_name_validation_rejects_unsafe_segments() {
        assert!(validate_provider_name("openai").is_ok());
        assert!(validate_provider_name("my-provider_2").is_ok());
        assert!(validate_provider_name("").is_err());
        assert!(validate_provider_name("a/b").is_err());
        assert!(validate_provider_name("a?x").is_err());
        assert!(validate_provider_name("a#x").is_err());
        assert!(validate_provider_name("a\\b").is_err());
        assert!(validate_provider_name("a\nb").is_err());
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify denied requests while fixture failures propagate"
    )]
    async fn apply_rejects_invalid_provider_name_without_request() -> anyhow::Result<()> {
        let (url, state) = spawn_mock(vec![]).await?;
        let (admin_env, key_env, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        std::env::set_var(&key_env, "sk");
        for bad in ["a/b", "a?x", "a#x", "a\\b", "a\nb", ""] {
            let change = Change::Rollback {
                name: bad.to_string(),
                revision: "r".to_string(),
            };
            let error = apply(&instance(&url, &admin_env), &change)
                .await
                .err()
                .ok_or_else(|| anyhow::anyhow!("Expected invalid-name rejection"))?;
            assert!(
                error.to_string().contains("provider name"),
                "for {bad:?}: {error}"
            );
        }
        let requests = state.requests.lock().await.clone();
        assert!(
            requests.is_empty(),
            "no request must be issued for an invalid provider name"
        );
        Ok(())
    }

    // -- inventory ----------------------------------------------------------

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify authenticated inventory while fixture failures propagate"
    )]
    async fn inventory_lists_sources_with_bearer_auth() -> anyhow::Result<()> {
        let (url, state) = spawn_mock(vec![route(
            "GET",
            "/admin/providers/sources",
            200,
            sources_json(&[("openai", "openai-compatible", "admin", Some("r1"), true)]),
        )])
        .await?;
        let (admin_env, _, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        let sources = inventory(&instance(&url, &admin_env)).await?;
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "openai");
        assert_eq!(sources[0].revision.as_deref(), Some("r1"));
        let requests = state.requests.lock().await.clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/admin/providers/sources");
        assert_eq!(
            requests[0].headers.get("authorization"),
            Some(&"Bearer admin-secret".to_string())
        );
        Ok(())
    }

    // -- add ----------------------------------------------------------------

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify create-only safety while fixture failures propagate"
    )]
    async fn add_refuses_existing_name_in_inventory() -> anyhow::Result<()> {
        let (url, state) = spawn_mock(vec![route(
            "GET",
            "/admin/providers/sources",
            200,
            sources_json(&[("openai", "openai-compatible", "admin", Some("r1"), true)]),
        )])
        .await?;
        let (admin_env, key_env, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        std::env::set_var(&key_env, "sk-existing");
        let change = Change::Add {
            name: "openai".to_string(),
            provider_type: "openai-compatible".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            key_env: key_env.clone(),
        };
        let err = apply(&instance(&url, &admin_env), &change)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("Expected existing-name rejection"))?;
        assert!(err.to_string().contains("already exists"));
        let requests = state.requests.lock().await.clone();
        assert!(
            !requests.iter().any(|r| r.path == "/admin/providers/create"),
            "must not POST create when the name already exists"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify provider creation while fixture failures propagate"
    )]
    async fn add_creates_provider_on_create_endpoint() -> anyhow::Result<()> {
        let (url, state) = spawn_mock(vec![
            route("GET", "/admin/providers/sources", 200, sources_json(&[])),
            route(
                "POST",
                "/admin/providers/create",
                200,
                serde_json::to_vec(&serde_json::json!({
                    "name": "openai", "enabled": true, "updated_at": "r1"
                }))?,
            ),
        ])
        .await?;
        let (admin_env, key_env, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        std::env::set_var(&key_env, "sk-new");
        let change = Change::Add {
            name: "openai".to_string(),
            provider_type: "openai-compatible".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            key_env: key_env.clone(),
        };
        apply(&instance(&url, &admin_env), &change).await?;
        let requests = state.requests.lock().await.clone();
        let create = requests
            .iter()
            .find(|r| r.path == "/admin/providers/create")
            .ok_or_else(|| anyhow::anyhow!("Missing create request"))?;
        assert_eq!(create.method, "POST");
        let body: serde_json::Value = serde_json::from_slice(&create.body)?;
        assert_eq!(body["name"], "openai");
        assert_eq!(body["config"]["type"], "openai-compatible");
        assert_eq!(body["config"]["api_key"], "sk-new");
        assert!(body.get("create_only").is_none(), "no create_only field");
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify local unauthenticated upstream support while fixtures propagate errors"
    )]
    async fn add_allows_empty_key_env_for_loopback_upstream() -> anyhow::Result<()> {
        let (url, state) = spawn_mock(vec![
            route("GET", "/admin/providers/sources", 200, sources_json(&[])),
            route(
                "POST",
                "/admin/providers/create",
                200,
                serde_json::to_vec(&serde_json::json!({"name":"local","enabled":true}))?,
            ),
        ])
        .await?;
        let (admin_env, key_env, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        std::env::set_var(&key_env, "unused");
        let change = Change::Add {
            name: "local".to_string(),
            provider_type: "openai-compatible".to_string(),
            base_url: "http://127.0.0.1:9999/v1".to_string(),
            key_env: String::new(),
        };
        apply(&instance(&url, &admin_env), &change).await?;
        let requests = state.requests.lock().await.clone();
        let create = requests
            .iter()
            .find(|r| r.path == "/admin/providers/create")
            .ok_or_else(|| anyhow::anyhow!("Missing local-provider create request"))?;
        let body: serde_json::Value = serde_json::from_slice(&create.body)?;
        assert_eq!(body["config"]["api_key"], "");
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify missing-key denial while fixture failures propagate"
    )]
    async fn add_refuses_empty_key_env_for_remote_upstream() -> anyhow::Result<()> {
        let (url, state) = spawn_mock(vec![route(
            "GET",
            "/admin/providers/sources",
            200,
            sources_json(&[]),
        )])
        .await?;
        let (admin_env, _, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        let change = Change::Add {
            name: "openai".to_string(),
            provider_type: "openai-compatible".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            key_env: String::new(),
        };
        let err = apply(&instance(&url, &admin_env), &change)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("Expected missing-key rejection"))?;
        assert!(err.to_string().contains("--key-env"));
        let requests = state.requests.lock().await.clone();
        assert!(
            !requests.iter().any(|r| r.path == "/admin/providers/create"),
            "must not POST create when key env is empty for a remote upstream"
        );
        Ok(())
    }

    // -- rotate / rollback --------------------------------------------------

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify pinned CAS revisions while fixture failures propagate"
    )]
    async fn rotate_uses_pinned_revision_and_resolved_key() -> anyhow::Result<()> {
        let (url, state) = spawn_mock(vec![route(
            "POST",
            "/admin/providers/openai/credentials",
            200,
            serde_json::to_vec(&serde_json::json!({"name":"openai","revision":"r2"}))?,
        )])
        .await?;
        let (admin_env, key_env, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        std::env::set_var(&key_env, "sk-rotated");
        let change = Change::Rotate {
            name: "openai".to_string(),
            revision: "r1".to_string(),
            key_env: key_env.clone(),
        };
        apply(&instance(&url, &admin_env), &change).await?;
        let requests = state.requests.lock().await.clone();
        assert_eq!(requests.len(), 1, "no re-fetch for rotate");
        assert_eq!(requests[0].path, "/admin/providers/openai/credentials");
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body)?;
        assert_eq!(body["expected_revision"], "r1");
        assert_eq!(body["api_key"], "sk-rotated");
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify conflict stops retries while fixture failures propagate"
    )]
    async fn rotate_conflict_is_reported_without_retry() -> anyhow::Result<()> {
        let (url, state) = spawn_mock(vec![route(
            "POST",
            "/admin/providers/openai/credentials",
            409,
            br#"{"error":"revision changed","code":"provider_revision_conflict"}"#.to_vec(),
        )])
        .await?;
        let (admin_env, key_env, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        std::env::set_var(&key_env, "sk-stale");
        let change = Change::Rotate {
            name: "openai".to_string(),
            revision: "old".to_string(),
            key_env: key_env.clone(),
        };
        let err = apply(&instance(&url, &admin_env), &change)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("Expected revision conflict"))?;
        assert!(err.to_string().contains("revision_conflict"));
        let requests = state.requests.lock().await.clone();
        assert_eq!(requests.len(), 1, "no auto retry on conflict");
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify rollback CAS while fixture failures propagate"
    )]
    async fn rollback_uses_pinned_revision() -> anyhow::Result<()> {
        let (url, state) = spawn_mock(vec![route(
            "POST",
            "/admin/providers/openai/credentials/rollback",
            200,
            serde_json::to_vec(&serde_json::json!({"name":"openai","revision":"r0"}))?,
        )])
        .await?;
        let (admin_env, _, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        let change = Change::Rollback {
            name: "openai".to_string(),
            revision: "r1".to_string(),
        };
        apply(&instance(&url, &admin_env), &change).await?;
        let requests = state.requests.lock().await.clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].path,
            "/admin/providers/openai/credentials/rollback"
        );
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body)?;
        assert_eq!(body["expected_revision"], "r1");
        assert!(body.get("api_key").is_none());
        Ok(())
    }

    // -- transport behavior -------------------------------------------------

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify redirect denial while fixture failures propagate"
    )]
    async fn redirects_are_rejected() -> anyhow::Result<()> {
        let (url, _state) =
            spawn_mock(vec![route("GET", "/admin/providers/sources", 302, vec![])]).await?;
        let (admin_env, _, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        let err = inventory(&instance(&url, &admin_env))
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("Expected redirect rejection"))?;
        assert!(err.to_string().contains("redirect"));
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify bounded response handling while fixture failures propagate"
    )]
    async fn oversized_response_is_rejected() -> anyhow::Result<()> {
        let mut big = route("GET", "/admin/providers/sources", 200, vec![1, 2, 3]);
        big.declared_length = Some(MAX_RESPONSE_BYTES + 1);
        let (url, _state) = spawn_mock(vec![big]).await?;
        let (admin_env, _, _) = hosts(&url);
        std::env::set_var(&admin_env, "admin-secret");
        let err = inventory(&instance(&url, &admin_env))
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("Expected response-size rejection"))?;
        assert!(err.to_string().contains("exceeds"));
        Ok(())
    }
}
