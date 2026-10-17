//! Portable intent only: no authority, catalog, accounting, credential resolution or Git execution.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};
#[path = "portable/cli.rs"]
pub mod cli;
#[path = "portable/connectivity.rs"]
mod connectivity;
pub use connectivity::{observe, Observations};
#[cfg(test)]
#[path = "portable/cli_tests.rs"]
mod cli_tests;
#[path = "portable/storage.rs"]
mod storage;
#[cfg(test)]
#[path = "portable/tests.rs"]
mod tests;

pub const MAX_BYTES: usize = 256 * 1024;
const MAX_ITEMS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connectivity {
    Public,
    Local,
    Tailnet,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub origin: String,
    pub connectivity: Connectivity,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub id: String,
    pub provider: String,
    pub model: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Loadout {
    pub id: String,
    pub routes: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub loadout: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub version: u32,
    pub providers: Vec<Provider>,
    pub routes: Vec<Route>,
    pub loadouts: Vec<Loadout>,
    pub profiles: Vec<Profile>,
}
/// Separate machine-local file. Values are environment variable *names*, never values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub provider: String,
    pub env: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bindings {
    pub version: u32,
    pub bindings: Vec<Binding>,
}
fn token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:-".contains(&b))
}
fn ids<'a>(mut items: impl Iterator<Item = &'a str>) -> bool {
    let mut seen = BTreeSet::new();
    items.all(|s| token(s) && seen.insert(s))
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > MAX_BYTES {
        bail!("portable document exceeds limit");
    }
    serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid portable document (details redacted)"))
}
pub fn parse(bytes: &[u8]) -> Result<Declaration> {
    let d: Declaration = decode(bytes)?;
    let valid = d.version == 1
        && [
            d.providers.len(),
            d.routes.len(),
            d.loadouts.len(),
            d.profiles.len(),
        ]
        .iter()
        .all(|n| *n <= MAX_ITEMS)
        && ids(d.providers.iter().map(|p| p.id.as_str()))
        && ids(d.routes.iter().map(|r| r.id.as_str()))
        && ids(d.loadouts.iter().map(|l| l.id.as_str()))
        && ids(d.profiles.iter().map(|p| p.id.as_str()));
    if !valid {
        bail!("invalid portable declaration structure");
    }
    for p in &d.providers {
        let u =
            url::Url::parse(&p.origin).map_err(|_| anyhow::anyhow!("invalid provider origin"))?;
        let host = u.host_str().unwrap_or_default().trim_matches(['[', ']']);
        let loopback = host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if p.origin.len() > 512
            || !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
            || u.path() != "/"
            || host.is_empty()
            || !(u.scheme() == "https"
                || (p.connectivity == Connectivity::Local && loopback && u.scheme() == "http"))
            || (p.connectivity == Connectivity::Local && !loopback)
        {
            bail!("invalid provider origin");
        }
    }
    for r in &d.routes {
        if !token(&r.model) || !d.providers.iter().any(|p| p.id == r.provider) {
            bail!("invalid route reference");
        }
    }
    for l in &d.loadouts {
        if l.routes.len() > MAX_ITEMS
            || !ids(l.routes.iter().map(String::as_str))
            || !l.routes.iter().all(|r| d.routes.iter().any(|x| &x.id == r))
        {
            bail!("invalid loadout selection");
        }
    }
    if !d
        .profiles
        .iter()
        .all(|p| d.loadouts.iter().any(|l| l.id == p.loadout))
    {
        bail!("invalid profile reference");
    }
    Ok(d)
}
pub fn parse_bindings(bytes: &[u8], d: &Declaration) -> Result<Bindings> {
    let b: Bindings = decode(bytes)?;
    if b.version != 1
        || b.bindings.len() > MAX_ITEMS
        || !ids(b.bindings.iter().map(|b| b.provider.as_str()))
        || !b.bindings.iter().all(|b| {
            let mut chars = b.env.bytes();
            b.env.len() <= 128
                && chars
                    .next()
                    .is_some_and(|c| c == b'_' || c.is_ascii_uppercase())
                && chars.all(|c| c == b'_' || c.is_ascii_uppercase() || c.is_ascii_digit())
                && d.providers.iter().any(|p| p.id == b.provider)
        })
    {
        bail!("invalid machine-local environment binding");
    }
    Ok(b)
}
/// Bounded explicit file read. No referenced file or environment resolution.
pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    storage::read(path)
}

pub fn preview_bindings(path: &Path, candidate: &[u8], d: &Declaration) -> Result<Preview> {
    parse_bindings(candidate, d)?;
    let before = storage::read(path)?;
    if path.exists() {
        parse_bindings(&before, d)?;
    }
    Ok(Preview {
        expected_revision: revision(&before),
        candidate_revision: revision(candidate),
    })
}

/// Publishes only reference names, under the same CAS/last-good contract as declarations.
pub fn apply_bindings(
    path: &Path,
    candidate: &[u8],
    review: &Preview,
    d: &Declaration,
) -> Result<()> {
    parse_bindings(candidate, d)?;
    if revision(candidate) != review.candidate_revision {
        bail!("candidate changed; preview again");
    }
    storage::apply(path, candidate, &review.expected_revision, |bytes| {
        parse_bindings(bytes, d).map(|_| ())
    })
}

pub fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub expected_revision: String,
    pub candidate_revision: String,
}
pub fn preview(path: &Path, candidate: &[u8]) -> Result<Preview> {
    parse(candidate)?;
    let before = storage::read(path)?;
    if path.exists() {
        parse(&before)?;
    }
    Ok(Preview {
        expected_revision: revision(&before),
        candidate_revision: revision(candidate),
    })
}
/// Revalidates pinned candidate and current bytes. No network or Git operation.
pub fn apply(path: &Path, candidate: &[u8], preview: &Preview) -> Result<()> {
    parse(candidate)?;
    if revision(candidate) != preview.candidate_revision {
        bail!("candidate changed; preview again");
    }
    storage::apply(path, candidate, &preview.expected_revision, |bytes| {
        parse(bytes).map(|_| ())
    })
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    Reachable,
    Unreachable,
    Unknown,
}
/// Transport observation, not admission or VPN membership evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub provider: String,
    pub state: Reachability,
    pub observed_at: u64,
}
#[derive(Debug, Serialize)]
pub struct Selection {
    pub route: String,
    pub state: Reachability,
}
/// Returns only caller-selected routes within the profile. Never retries or substitutes.
pub fn plan(
    d: &Declaration,
    profile: &str,
    selected: &[String],
    observations: &Observations,
    now: u64,
) -> Result<Vec<Selection>> {
    // Public structs may have been constructed without parsing.
    let validated = serde_json::to_vec(d)?;
    parse(&validated)?;
    observations.validate(d)?;
    if selected.is_empty()
        || selected.len() > MAX_ITEMS
        || !ids(selected.iter().map(String::as_str))
    {
        bail!("invalid plan input");
    }
    let p = d
        .profiles
        .iter()
        .find(|p| p.id == profile)
        .ok_or_else(|| anyhow::anyhow!("unknown profile"))?;
    let l = d
        .loadouts
        .iter()
        .find(|l| l.id == p.loadout)
        .ok_or_else(|| anyhow::anyhow!("unknown loadout"))?;
    let mut result = Vec::new();
    for id in selected {
        if !l.routes.contains(id) {
            bail!("route not explicitly approved in profile");
        }
        let r = d
            .routes
            .iter()
            .find(|r| &r.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown route"))?;
        let state = observations
            .items
            .iter()
            .find(|o| o.provider == r.provider)
            .filter(|o| {
                now.checked_sub(o.observed_at)
                    .is_some_and(|age| age <= observations.ttl_seconds)
            })
            .map_or(Reachability::Unknown, |o| o.state);
        result.push(Selection {
            route: id.clone(),
            state,
        });
    }
    Ok(result)
}
