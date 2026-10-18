//! Explicit device connections. Discovery is observation, never authorization or inference.
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(test)]
mod e2e_tests;
mod invitation;
#[cfg(test)]
mod listener_tests;
mod pi_config;
pub mod portable;
mod storage;
#[cfg(test)]
mod tests;

#[derive(Args)]
pub struct DeviceArgs {
    /// Machine-local registry (never a credential file).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// On apply, pin the candidate bytes returned by preview.
    #[arg(long, global = true)]
    pub candidate_revision: Option<String>,
    #[command(subcommand)]
    pub command: DeviceCommand,
}
#[derive(Subcommand)]
pub enum DeviceCommand {
    /// Issue a one-time typed Oqto invitation into private JSON/QR files (never stdout).
    Invite(invitation::InviteArgs),
    /// Secret-free declarations and explicit selection plans, not serving authority.
    Portable {
        #[command(subcommand)]
        command: portable::cli::Command,
    },
    /// Observe scoped live catalogs; never falls back to cached inventory.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Preview enrollment using an already-issued scoped virtual key reference.
    #[command(visible_alias = "enroll")]
    Connect {
        #[arg(long)]
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        key_env: String,
        /// Optional machine-local listener capability reference (X-Eavs-Token).
        #[arg(long)]
        listener_key_env: Option<String>,
        /// Explicitly trust this gateway to receive the referenced virtual key.
        #[arg(long)]
        trust: bool,
        /// Required for plaintext HTTP to a literal loopback address.
        #[arg(long)]
        allow_loopback_http: bool,
        /// Apply only against the revision returned by a previous preview.
        #[arg(long)]
        expected_revision: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Export scoped Pi provider definitions for dynamic registration; no config writes.
    PiModels {
        #[arg(long)]
        name: String,
    },
    /// Preview/apply a scoped live Pi models.json; never includes credential values.
    Pi {
        #[arg(long)]
        name: String,
        /// Explicit target; no implicit global/workspace configuration edit.
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        expected_revision: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Preview/apply a scoped inventory snapshot, not provider config or credentials.
    Sync {
        #[arg(long)]
        name: String,
        #[arg(long)]
        expected_revision: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}
#[derive(Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub connections: Vec<Connection>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub name: String,
    pub url: String,
    pub key_env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listener_key_env: Option<String>,
    pub allow_loopback_http: bool,
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub name: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub models: Vec<Model>,
}
#[derive(Serialize)]
pub struct Status {
    pub name: String,
    pub url: String,
    pub state: &'static str,
    pub diagnostic: &'static str,
    pub providers: Vec<Provider>,
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && s != "."
        && s != ".."
}
fn text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control)
}
fn environment_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}
fn validate(c: &Connection) -> Result<()> {
    if !identifier(&c.name)
        || !environment_reference(&c.key_env)
        || c.listener_key_env
            .as_deref()
            .is_some_and(|name| !environment_reference(name))
    {
        bail!("invalid connection name or environment reference");
    }
    let u = reqwest::Url::parse(&c.url).map_err(|_| anyhow::anyhow!("invalid gateway URL"))?;
    let loopback = u
        .host_str()
        .is_some_and(|h| matches!(h, "127.0.0.1" | "[::1]" | "::1"));
    if u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path() != "/"
        || !(u.scheme() == "https" || (u.scheme() == "http" && loopback && c.allow_loopback_http))
    {
        bail!("gateway requires HTTPS origin, or explicitly allowed literal loopback HTTP; no userinfo, path, query or fragment");
    }
    Ok(())
}
fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn load(path: &Path) -> Result<(Registry, Vec<u8>)> {
    let bytes = storage::read(path)?;
    if bytes.is_empty() && path.exists() {
        bail!("empty device registry; left unchanged");
    }
    let r: Registry = if bytes.is_empty() {
        Registry::default()
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("malformed device registry; left unchanged"))?
    };
    if r.connections.len() > 32 {
        bail!("too many connections");
    }
    let mut names = BTreeSet::new();
    for c in &r.connections {
        validate(c)?;
        if !names.insert(&c.name) {
            bail!("duplicate connection");
        }
    }
    Ok((r, bytes))
}
/// Safe public catalog projection. Unknown metadata is discarded, never persisted.
fn project(bytes: &[u8]) -> Result<Vec<Provider>> {
    let v: serde_json::Value = serde_json::from_slice(bytes).context("invalid catalog")?;
    if v["schema_version"] != 1 || v["scope"] != "key-filtered-chat-candidates" {
        bail!("unsupported catalog");
    }
    let ps = v["providers"].as_array().context("missing providers")?;
    if ps.len() > 256 {
        bail!("catalog limit");
    }
    let mut count = 0;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for p in ps {
        let id = p["id"]
            .as_str()
            .filter(|s| identifier(s))
            .context("invalid provider")?;
        if !seen.insert(id) {
            bail!("duplicate provider");
        }
        let mut models = Vec::new();
        let mut ids = BTreeSet::new();
        for m in p["models"].as_array().context("missing models")? {
            count += 1;
            if count > 4096 {
                bail!("catalog limit");
            }
            let id = m["id"]
                .as_str()
                .filter(|s| text(s))
                .context("invalid model")?;
            let name = m["name"]
                .as_str()
                .filter(|s| text(s))
                .context("invalid model name")?;
            if !ids.insert(id) {
                bail!("duplicate model");
            }
            models.push(Model {
                id: id.into(),
                name: name.into(),
            });
        }
        out.push(Provider {
            id: id.into(),
            models,
        });
    }
    Ok(out)
}
const fn http_failure(code: reqwest::StatusCode) -> (&'static str, &'static str) {
    match code.as_u16() {
        401 | 403 => ("unauthorized", "authentication_rejected"),
        429 => ("unavailable", "rate_limited"),
        500..=599 => ("unavailable", "server_failure"),
        _ => ("unavailable", "unexpected_http_status"),
    }
}
/// No proxy, redirects, secret-file reads, commands, inference, retries or fallback.
pub async fn observe(c: &Connection) -> Result<Status> {
    validate(c)?;
    let s = Status {
        name: c.name.clone(),
        url: c.url.clone(),
        state: "unconfigured",
        diagnostic: "credential_missing",
        providers: vec![],
    };
    let Ok(key) = std::env::var(&c.key_env) else {
        return Ok(s);
    };
    observe_key(c, &key, s).await
}
async fn observe_key(c: &Connection, key: &str, mut s: Status) -> Result<Status> {
    if !crate::keys::is_virtual_key(key) {
        s.diagnostic = "scoped_key_required";
        return Ok(s);
    }
    let listener = match c.listener_key_env.as_deref() {
        Some(reference) => match std::env::var(reference) {
            Ok(value) if !value.is_empty() && http::HeaderValue::from_str(&value).is_ok() => {
                Some(value)
            }
            _ => {
                s.diagnostic = "listener_credential_missing_or_invalid";
                return Ok(s);
            }
        },
        None => None,
    };
    observe_http(c, key, listener.as_deref(), s).await
}
async fn observe_http(
    c: &Connection,
    key: &str,
    listener: Option<&str>,
    mut s: Status,
) -> Result<Status> {
    match catalog_http(c, key, listener).await {
        Ok(bytes) => {
            s.providers = project(&bytes)?;
            s.state = "ready";
            s.diagnostic = "catalog_available";
        }
        Err((state, diagnostic)) => {
            s.state = state;
            s.diagnostic = diagnostic;
        }
    }
    Ok(s)
}
async fn catalog_http(
    c: &Connection,
    key: &str,
    listener: Option<&str>,
) -> std::result::Result<Vec<u8>, (&'static str, &'static str)> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| ("unavailable", "client_unavailable"))?;
    let url = format!("{}/v1/eavs/catalog", c.url.trim_end_matches('/'));
    let request = async {
        let mut builder = client.get(url).bearer_auth(key);
        if let Some(token) = listener {
            builder = builder.header("x-eavs-token", token);
        }
        let mut response = builder
            .send()
            .await
            .map_err(|_| ("unreachable", "transport_failure"))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(http_failure(response.status()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ("unreachable", "transport_failure"))?
        {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err(("unavailable", "catalog_limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        project(&bytes).map_err(|_| ("unavailable", "invalid_catalog"))?;
        Ok(bytes)
    };
    tokio::time::timeout(Duration::from_secs(6), request)
        .await
        .unwrap_or(Err(("unreachable", "transport_timeout")))
}
async fn catalog(c: &Connection) -> Result<serde_json::Value> {
    validate(c)?;
    let key =
        std::env::var(&c.key_env).map_err(|_| anyhow::anyhow!("device credential unavailable"))?;
    if !crate::keys::is_virtual_key(&key) {
        bail!("scoped virtual key required");
    }
    let listener = c
        .listener_key_env
        .as_deref()
        .map(|reference| {
            std::env::var(reference).map_err(|_| anyhow::anyhow!("listener credential unavailable"))
        })
        .transpose()?;
    if listener
        .as_deref()
        .is_some_and(|value| value.is_empty() || http::HeaderValue::from_str(value).is_err())
    {
        bail!("listener credential invalid");
    }
    let bytes = catalog_http(c, &key, listener.as_deref())
        .await
        .map_err(|(_, code)| anyhow::anyhow!("catalog unavailable: {code}"))?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("catalog invalid"))
}
fn check_candidate(candidate: &[u8], expected: Option<&str>) -> Result<()> {
    if expected != Some(revision(candidate).as_str()) {
        bail!("candidate changed or not approved; use --candidate-revision from a fresh preview");
    }
    Ok(())
}
async fn execute(args: DeviceArgs) -> Result<serde_json::Value> {
    if let DeviceCommand::Portable { command } = args.command {
        return portable::cli::execute(command, args.candidate_revision.as_deref()).await;
    }
    execute_connections(args).await
}
async fn execute_connections(args: DeviceArgs) -> Result<serde_json::Value> {
    let path = match args.config {
        Some(p) => p,
        None => crate::paths::config_dir()?.join("eavs/device.json"),
    };
    let (mut registry, before) = load(&path)?;
    match args.command {
        DeviceCommand::Portable { command } => {
            portable::cli::execute(command, args.candidate_revision.as_deref()).await
        }
        DeviceCommand::Invite(_) => bail!("device invite is a CLI-only administrator operation"),
        DeviceCommand::Status { .. } => {
            let mut statuses = Vec::new();
            for c in &registry.connections {
                statuses.push(observe(c).await?);
            }
            Ok(serde_json::json!({"schema_version":1,"connections":statuses}))
        }
        DeviceCommand::Connect {
            name,
            url,
            key_env,
            listener_key_env,
            trust,
            allow_loopback_http,
            expected_revision,
            yes,
        } => {
            let c = Connection {
                name,
                url,
                key_env,
                listener_key_env,
                allow_loopback_http,
            };
            let candidate = prepare_connection(c, &mut registry, trust).await?;
            let current = revision(&before);
            if yes {
                check_candidate(&candidate, args.candidate_revision.as_deref())?;
                storage::apply(&path, &before, &candidate, expected_revision.as_deref())?;
            }
            Ok(
                serde_json::json!({"schema_version":1,"applied":yes,"revision":current,"candidate_revision":revision(&candidate),"connections":registry.connections}),
            )
        }
        DeviceCommand::PiModels { name } => {
            let c = registry
                .connections
                .iter()
                .find(|c| c.name == name)
                .context("unknown connection")?;
            pi_config::export(c, &catalog(c).await?)
        }
        DeviceCommand::Pi {
            name,
            output,
            expected_revision,
            yes,
        } => {
            let c = registry
                .connections
                .iter()
                .find(|c| c.name == name)
                .context("unknown connection")?;
            let live = catalog(c).await?;
            pi_config::configure(
                c,
                &live,
                &pi_config::Publication {
                    output: &output,
                    registry_before: &before,
                    registry_path: &path,
                    expected: expected_revision.as_deref(),
                    candidate: args.candidate_revision.as_deref(),
                    apply: yes,
                },
            )
        }
        DeviceCommand::Sync {
            name,
            expected_revision,
            yes,
        } => {
            let c = registry
                .connections
                .iter()
                .find(|c| c.name == name)
                .context("unknown connection")?;
            sync_inventory(
                c,
                &path,
                &before,
                expected_revision.as_deref(),
                args.candidate_revision.as_deref(),
                yes,
            )
            .await
        }
    }
}
async fn prepare_connection(
    c: Connection,
    registry: &mut Registry,
    trust: bool,
) -> Result<Vec<u8>> {
    if !trust {
        bail!("connect requires --trust for the explicitly chosen gateway");
    }
    validate(&c)?;
    let status = observe(&c).await?;
    if status.state != "ready" {
        bail!("connection not ready: {}", status.diagnostic);
    }
    registry.connections.retain(|old| old.name != c.name);
    registry.connections.push(c);
    if registry.connections.len() > 32 {
        bail!("too many connections");
    }
    Ok(serde_json::to_vec_pretty(registry)?)
}
async fn sync_inventory(
    c: &Connection,
    path: &Path,
    before: &[u8],
    expected: Option<&str>,
    approved: Option<&str>,
    apply: bool,
) -> Result<serde_json::Value> {
    let status = observe(c).await?;
    if status.state != "ready" {
        bail!(
            "sync unavailable: {}; last-good unchanged",
            status.diagnostic
        );
    }
    let target = path.with_file_name(format!("device-inventory-{}.json", c.name));
    let old = storage::read(&target)?;
    if old.is_empty() && target.exists() {
        bail!("empty inventory; left unchanged");
    }
    validate_inventory(&old, c)?;
    let inventory = Inventory {
        schema_version: 1,
        name: c.name.clone(),
        url: c.url.clone(),
        providers: status.providers,
    };
    let candidate = serde_json::to_vec_pretty(&inventory)?;
    if apply {
        check_candidate(&candidate, approved)?;
        storage::with_snapshot(path, before, || {
            storage::apply(&target, &old, &candidate, expected)
        })?;
    }
    Ok(
        serde_json::json!({"schema_version":1,"applied":apply,"revision":revision(&old),"candidate_revision":revision(&candidate),"inventory":inventory}),
    )
}
fn validate_inventory(bytes: &[u8], connection: &Connection) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let previous: Inventory = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("malformed inventory; left unchanged"))?;
    if previous.schema_version != 1
        || previous.name != connection.name
        || previous.url != connection.url
    {
        bail!("inventory identity conflict");
    }
    // Reuse the strict projection checks for stored public identifiers and limits.
    let value = serde_json::json!({"schema_version":1,"scope":"key-filtered-chat-candidates","providers":previous.providers});
    project(&serde_json::to_vec(&value)?)?;
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    schema_version: u8,
    name: String,
    url: String,
    providers: Vec<Provider>,
}

pub async fn run_cli(args: DeviceArgs) -> Result<()> {
    let args = match args.command {
        DeviceCommand::Invite(options) => {
            return invitation::run_cli(
                options,
                args.config.is_some() || args.candidate_revision.is_some(),
            )
            .await;
        }
        command => DeviceArgs { command, ..args },
    };
    let human = matches!(args.command, DeviceCommand::Status { json: false });
    match execute(args).await {
        Ok(value) => {
            if human {
                if let Some(items) = value["connections"].as_array() {
                    for s in items {
                        println!(
                            "{}: {} ({})",
                            s["name"].as_str().unwrap_or_default(),
                            s["state"].as_str().unwrap_or_default(),
                            s["diagnostic"].as_str().unwrap_or_default()
                        );
                    }
                }
            } else {
                println!("{}", serde_json::to_string(&value)?);
            }
            Ok(())
        }
        Err(e) => {
            println!(
                "{}",
                serde_json::json!({"schema_version":1,"error":{"code":"device_operation_failed","message":e.to_string()}})
            );
            Err(e)
        }
    }
}
