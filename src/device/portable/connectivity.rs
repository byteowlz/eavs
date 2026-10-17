//! Explicit HEAD-only transport checks: no credentials, response bodies or inference.
use super::{ids, parse, revision, Declaration, Observation, Reachability, MAX_ITEMS};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observations {
    pub version: u32,
    /// Pin observations to validated declaration content, including origins.
    pub declaration_revision: String,
    pub ttl_seconds: u64,
    pub items: Vec<Observation>,
}
impl Observations {
    pub fn empty(d: &Declaration) -> Result<Self> {
        Ok(Self {
            version: 1,
            declaration_revision: fingerprint(d)?,
            ttl_seconds: 30,
            items: Vec::new(),
        })
    }
    pub(super) fn validate(&self, d: &Declaration) -> Result<()> {
        if self.version != 1
            || !(1..=60).contains(&self.ttl_seconds)
            || self.declaration_revision != fingerprint(d)?
            || self.items.len() > MAX_ITEMS
            || !ids(self.items.iter().map(|o| o.provider.as_str()))
            || !self
                .items
                .iter()
                .all(|o| d.providers.iter().any(|p| p.id == o.provider))
        {
            bail!("invalid observation document or declaration revision");
        }
        Ok(())
    }
}
fn fingerprint(d: &Declaration) -> Result<String> {
    let bytes = serde_json::to_vec(d)?;
    parse(&bytes)?;
    Ok(revision(&bytes))
}
pub fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| anyhow::anyhow!("system clock precedes epoch"))
}
/// Caller explicitly approves each origin by provider ID. Maximum 8 requests,
/// 2 seconds each plus a 17-second enclosing deadline. No proxies/redirects/retries.
/// Any HTTP status proves only an HTTP response, never inference readiness.
pub async fn observe(
    d: &Declaration,
    providers: &[String],
    ttl_seconds: u64,
) -> Result<Observations> {
    let mut out = Observations::empty(d)?;
    out.ttl_seconds = ttl_seconds;
    out.validate(d)?;
    if providers.is_empty()
        || providers.len() > 8
        || !ids(providers.iter().map(String::as_str))
        || !providers
            .iter()
            .all(|id| d.providers.iter().any(|p| &p.id == id))
    {
        bail!("observe requires 1..8 explicit known provider IDs");
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(2))
        .build()?;
    let work = async {
        for id in providers {
            let p = d
                .providers
                .iter()
                .find(|p| &p.id == id)
                .ok_or_else(|| anyhow::anyhow!("unknown provider"))?;
            let state = if client.head(&p.origin).send().await.is_ok() {
                Reachability::Reachable
            } else {
                Reachability::Unreachable
            };
            out.items.push(Observation {
                provider: id.clone(),
                state,
                observed_at: now()?,
            });
        }
        Ok(out)
    };
    tokio::time::timeout(Duration::from_secs(17), work)
        .await
        .map_err(|_| {
            anyhow::anyhow!("observation deadline exceeded; no partial observations published")
        })?
}
