//! Model availability: operator and fleet-maintenance deactivations.
//!
//! GPU runtimes change loadouts; a configured model can stop existing on its
//! provider without anyone editing config. Deactivations live in
//! `$XDG_STATE_HOME/eavs/model-availability.json`, applied on top of config:
//! deactivated models vanish from catalogs and a direct request answers a
//! typed `model_unavailable` error with the reason and the remedy. Config is
//! never touched; enabling is one verb.
//!
//! The server is the single writer (CLI and fleet tools change availability
//! through `/eavs/maintenance/availability`), so state is in-memory with
//! best-effort persistence, like the learned-efforts store.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::warn;

/// Why a model is deactivated and who deactivated it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deactivation {
    /// Free-text reason, e.g. "loadout switch: coding loadout".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When the deactivation lapses (TTL); `None` = until re-enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    /// The maintenance key id or "operator".
    pub set_by: String,
    pub set_at: DateTime<Utc>,
}

impl Deactivation {
    fn expired(&self, now: DateTime<Utc>) -> bool {
        self.until.is_some_and(|until| now >= until)
    }
}

/// What a deactivated model answers with.
#[derive(Debug, Clone)]
pub struct Unavailable {
    pub reason: Option<String>,
    /// When it comes back on its own, if a TTL was set.
    pub until: Option<DateTime<Utc>>,
    pub set_by: String,
}

/// provider -> model -> deactivation.
type Entries = BTreeMap<String, BTreeMap<String, Deactivation>>;

/// Outcome of the last availability probe of a local runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeStatus {
    pub at: DateTime<Utc>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Model ids the runtime last reported serving.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub served: Vec<String>,
}

/// Persisted availability state: deactivations plus per-provider probe
/// bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    deactivations: Entries,
    #[serde(default)]
    probe: BTreeMap<String, ProbeStatus>,
}

/// Process-wide deactivations, persisted best-effort.
pub struct Availability {
    path: Option<PathBuf>,
    entries: RwLock<Entries>,
    probe: RwLock<BTreeMap<String, ProbeStatus>>,
}

/// One provider's slice of the snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderAvailability {
    pub provider: String,
    /// model -> deactivation detail.
    pub models: BTreeMap<String, Deactivation>,
}

impl Availability {
    pub fn load(path: Option<PathBuf>) -> Self {
        let persisted = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| {
                // v2: {deactivations, probe}. v1 files held the bare map.
                serde_json::from_slice::<Persisted>(&bytes)
                    .ok()
                    .or_else(|| {
                        serde_json::from_slice::<Entries>(&bytes)
                            .ok()
                            .map(|deactivations| Persisted {
                                deactivations,
                                probe: BTreeMap::new(),
                            })
                    })
            })
            .unwrap_or_default();
        Self {
            path,
            entries: RwLock::new(persisted.deactivations),
            probe: RwLock::new(persisted.probe),
        }
    }

    /// `$XDG_STATE_HOME/eavs/model-availability.json`, or `None` in tests.
    pub fn state_file() -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        Some(
            crate::paths::state_dir()
                .ok()?
                .join("eavs")
                .join("model-availability.json"),
        )
    }

    /// Why `model` on `provider` is deactivated right now, if it is. Expired
    /// TTLs are dropped lazily (and persisted on the next mutation).
    pub async fn unavailable(&self, provider: &str, model: &str) -> Option<Unavailable> {
        let now = Utc::now();
        let found = {
            let entries = self.entries.read().await;
            entries
                .get(provider)
                .and_then(|provider| provider.get(model))
                .and_then(|deactivation| {
                    (!deactivation.expired(now)).then(|| {
                        (
                            deactivation.reason.clone(),
                            deactivation.until,
                            deactivation.set_by.clone(),
                        )
                    })
                })
        };
        let (reason, until, set_by) = found?;
        Some(Unavailable {
            reason,
            until,
            set_by,
        })
    }

    /// All currently deactivated models, expired entries dropped.
    pub async fn snapshot(&self) -> Vec<ProviderAvailability> {
        let now = Utc::now();
        let entries = self.entries.read().await;
        entries
            .iter()
            .map(|(provider, models)| ProviderAvailability {
                provider: provider.clone(),
                models: models
                    .iter()
                    .filter(|(_, deactivation)| !deactivation.expired(now))
                    .map(|(model, deactivation)| (model.clone(), deactivation.clone()))
                    .collect(),
            })
            .filter(|provider| !provider.models.is_empty())
            .collect()
    }

    /// Deactivate `models` on `provider`. Returns how many changed.
    pub async fn deactivate(
        &self,
        provider: &str,
        models: &[String],
        reason: Option<String>,
        until: Option<DateTime<Utc>>,
        set_by: &str,
    ) -> usize {
        let mut changed = 0;
        let snapshot = {
            let mut entries = self.entries.write().await;
            let now = Utc::now();
            let slot = entries.entry(provider.to_owned()).or_default();
            for model in models {
                let deactivation = Deactivation {
                    reason: reason.clone(),
                    until,
                    set_by: set_by.to_owned(),
                    set_at: now,
                };
                if slot.get(model) != Some(&deactivation) {
                    changed += 1;
                }
                slot.insert(model.clone(), deactivation);
            }
            if changed > 0 {
                Some(entries.clone())
            } else {
                None
            }
        };
        if snapshot.is_some() {
            self.persist().await;
        }
        changed
    }

    /// Re-activate `models` on `provider` (`None` = all). Returns how many
    /// changed.
    pub async fn activate(&self, provider: &str, models: Option<&[String]>) -> usize {
        let mut changed = 0;
        let snapshot = {
            let mut entries = self.entries.write().await;
            let Some(slot) = entries.get_mut(provider) else {
                return 0;
            };
            if let Some(models) = models {
                for model in models {
                    changed += usize::from(slot.remove(model).is_some());
                }
            } else {
                changed = slot.len();
                slot.clear();
            }
            if slot.is_empty() {
                entries.remove(provider);
            }
            let snapshot = (changed > 0).then(|| entries.clone());
            drop(entries);
            snapshot
        };
        if snapshot.is_some() {
            self.persist().await;
        }
        changed
    }

    /// Loadout switch: `active` is the complete set of models the runtime
    /// serves right now, `universe` every model configured on the provider.
    /// Everything in the universe but not active is deactivated (refreshed
    /// with this announce's reason/TTL); deactivated models in `active` are
    /// re-activated — an announce overrides earlier deactivations. Returns
    /// how many models changed state.
    pub async fn replace_active(
        &self,
        provider: &str,
        active: &[String],
        universe: &[String],
        reason: Option<String>,
        until: Option<DateTime<Utc>>,
        set_by: &str,
    ) -> usize {
        let deactivated: Vec<String> = universe
            .iter()
            .filter(|model| !active.contains(model))
            .cloned()
            .collect();
        let mut changed = self
            .deactivate(provider, &deactivated, reason, until, set_by)
            .await;
        let mut activated = 0;
        let snapshot = {
            let mut entries = self.entries.write().await;
            let Some(slot) = entries.get_mut(provider) else {
                return changed;
            };
            for model in active {
                activated += usize::from(slot.remove(model).is_some());
            }
            let snapshot = (activated > 0).then(|| entries.clone());
            drop(entries);
            snapshot
        };
        changed += activated;
        if snapshot.is_some() {
            self.persist().await;
        }
        changed
    }

    /// Fold one local-runtime probe result into the store. `served` is what
    /// the runtime currently lists, `universe` every model it could serve
    /// (configured shortlist, else the union of previous and current probe
    /// results). Models in the universe but not served are deactivated with
    /// `set_by = "probe"`; probe entries never overwrite operator or
    /// maintenance deactivations. Models that are served again lose their
    /// probe deactivation; operator entries survive. Returns how many models
    /// were deactivated and re-activated by this probe.
    pub async fn apply_probe(
        &self,
        provider: &str,
        served: &[String],
        universe: &[String],
        error: Option<&str>,
    ) -> (usize, usize) {
        const PROBE: &str = "probe";
        let reason = error.map_or_else(
            || "Not served by the runtime (availability probe)".to_owned(),
            |error| format!("Runtime did not answer its model list: {error}"),
        );
        let mut deactivated = 0;
        let mut reactivated = 0;
        let persist_state = {
            let mut entries = self.entries.write().await;
            let now = Utc::now();
            let slot = entries.entry(provider.to_owned()).or_default();
            for model in universe {
                if served.contains(model) {
                    continue;
                }
                let probe_managed = slot
                    .get(model)
                    .is_none_or(|existing| existing.set_by == PROBE);
                let deactivation = Deactivation {
                    reason: Some(reason.clone()),
                    until: None,
                    set_by: PROBE.to_owned(),
                    set_at: now,
                };
                if probe_managed && slot.get(model) != Some(&deactivation) {
                    deactivated += 1;
                    slot.insert(model.clone(), deactivation);
                }
            }
            for model in served {
                let was_probe = matches!(
                    slot.get(model),
                    Some(Deactivation { set_by, .. }) if set_by == PROBE
                );
                reactivated += usize::from(was_probe);
                slot.remove(model);
            }
            if slot.is_empty() {
                entries.remove(provider);
            }
            let mut probe = self.probe.write().await;
            probe.insert(
                provider.to_owned(),
                ProbeStatus {
                    at: now,
                    ok: error.is_none(),
                    error: error.map(str::to_owned),
                    served: served.to_vec(),
                },
            );
            Persisted {
                deactivations: entries.clone(),
                probe: probe.clone(),
            }
        };
        let current = {
            let entries = self.entries.read().await;
            let deactivations = entries.clone();
            let probe_map = self.probe.read().await;
            let probe = probe_map.clone();
            drop(entries);
            drop(probe_map);
            Persisted {
                deactivations,
                probe,
            }
        };
        if current != persist_state {
            self.persist().await;
        }
        (deactivated, reactivated)
    }

    /// What the last probe saw a runtime serving, for the probe universe of
    /// providers that serve a full catalog instead of a shortlist.
    pub async fn last_served(&self, provider: &str) -> Vec<String> {
        self.probe
            .read()
            .await
            .get(provider)
            .map(|status| status.served.clone())
            .unwrap_or_default()
    }

    /// Last probe outcome per provider, for status displays.
    pub async fn probe_snapshot(&self) -> BTreeMap<String, ProbeStatus> {
        self.probe.read().await.clone()
    }

    async fn persist(&self) {
        let Some(path) = &self.path else { return };
        let bytes = {
            let entries = self.entries.read().await;
            let probe = self.probe.read().await;
            let state = Persisted {
                deactivations: entries.clone(),
                probe: probe.clone(),
            };
            drop(entries);
            drop(probe);
            serde_json::to_vec_pretty(&state).unwrap_or_default()
        };
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, bytes)?;
                std::fs::rename(&tmp, path)
            });
        if let Err(error) = result {
            warn!(%error, path = %path.display(), "Could not persist model availability");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Availability {
        Availability::load(None)
    }

    fn models<const N: usize>(ids: [&str; N]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[tokio::test]
    async fn deactivate_hides_until_enabled() {
        let availability = store();
        availability
            .deactivate(
                "rtx",
                &models(["qwen3.8", "kimi"]),
                Some("loadout".into()),
                None,
                "operator",
            )
            .await;
        assert!(availability.unavailable("rtx", "qwen3.8").await.is_some());
        assert!(availability.unavailable("rtx", "kimi").await.is_some());
        assert!(availability.unavailable("rtx", "other").await.is_none());
        assert!(availability.unavailable("other", "qwen3.8").await.is_none());

        let snapshot = availability.snapshot().await;
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].models.len(), 2);

        availability
            .activate("rtx", Some(&models(["qwen3.8"])))
            .await;
        assert!(availability.unavailable("rtx", "qwen3.8").await.is_none());
        assert!(availability.unavailable("rtx", "kimi").await.is_some());
        availability.activate("rtx", None).await;
        assert!(availability.snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn ttl_lapses_on_its_own() {
        let availability = store();
        let until = Utc::now() - chrono::Duration::seconds(1);
        availability
            .deactivate("rtx", &models(["qwen3.8"]), None, Some(until), "opengo-1")
            .await;
        assert!(availability.unavailable("rtx", "qwen3.8").await.is_none());
        assert!(availability.snapshot().await.is_empty());
        // Re-enabling drops the lapsed record: a real state change.
        assert_eq!(availability.activate("rtx", None).await, 1);
    }

    #[tokio::test]
    async fn replace_active_deactivates_everything_else() {
        let availability = store();
        availability
            .deactivate("rtx", &models(["old"]), None, None, "operator")
            .await;
        let universe = models(["old", "qwen3.8", "kimi", "ghost"]);
        let changed = availability
            .replace_active(
                "rtx",
                &models(["qwen3.8", "kimi"]),
                &universe,
                Some("loadout: coding".into()),
                None,
                "opengo-1",
            )
            .await;
        // old and ghost deactivated/refreshed; qwen3.8 and kimi are announced
        // active, so they gain no entry.
        assert_eq!(changed, 2);
        assert!(availability.unavailable("rtx", "old").await.is_some());
        assert!(availability.unavailable("rtx", "ghost").await.is_some());
        assert!(availability.unavailable("rtx", "kimi").await.is_none());
        assert!(availability.unavailable("rtx", "qwen3.8").await.is_none());

        // Switching back: only the announced set stays active.
        let changed = availability
            .replace_active(
                "rtx",
                &models(["qwen3.8"]),
                &universe,
                None,
                None,
                "opengo-1",
            )
            .await;
        // old/kimi refreshed with the new announce, ghost deactivated;
        // qwen3.8 was already active, so it counts nothing.
        assert_eq!(changed, 3);
        assert!(availability.unavailable("rtx", "qwen3.8").await.is_none());
        assert!(availability.unavailable("rtx", "old").await.is_some());
        assert!(availability.unavailable("rtx", "kimi").await.is_some());
        assert!(availability.unavailable("rtx", "ghost").await.is_some());
    }

    #[tokio::test]
    #[expect(
        clippy::unwrap_used,
        reason = "unavailable() results are asserted directly"
    )]
    async fn probe_folds_into_the_store() {
        let availability = store();
        // Operator hard-disables glm: the probe must never resurrect it.
        availability
            .deactivate(
                "rtx",
                &models(["glm"]),
                Some("operator choice".into()),
                None,
                "operator",
            )
            .await;
        let universe = models(["glm", "qwen3.8", "kimi"]);

        // Runtime serves qwen3.8 and kimi, not glm.
        let (deactivated, reactivated) = availability
            .apply_probe("rtx", &models(["qwen3.8", "kimi"]), &universe, None)
            .await;
        assert_eq!((deactivated, reactivated), (0, 0));
        assert!(availability.unavailable("rtx", "glm").await.is_some());

        // The loadout drops kimi: probe deactivates it, glm keeps the
        // operator's entry (not overwritten with a probe one).
        let (deactivated, reactivated) = availability
            .apply_probe("rtx", &models(["qwen3.8"]), &universe, None)
            .await;
        assert_eq!((deactivated, reactivated), (1, 0));
        let kimi = availability.unavailable("rtx", "kimi").await.unwrap();
        assert_eq!(kimi.set_by, "probe");
        assert_eq!(
            availability.unavailable("rtx", "glm").await.unwrap().set_by,
            "operator"
        );

        // kimi back (qwen3.8 left the loadout earlier, so it re-deactivates):
        // probe entries clear, operator entries survive anything.
        let (deactivated, reactivated) = availability
            .apply_probe("rtx", &models(["kimi"]), &universe, None)
            .await;
        assert_eq!((deactivated, reactivated), (1, 1));
        assert!(availability.unavailable("rtx", "kimi").await.is_none());
        assert_eq!(
            availability
                .unavailable("rtx", "qwen3.8")
                .await
                .unwrap()
                .set_by,
            "probe"
        );

        // Runtime unreachable: probe-managed models deactivate, operator
        // entries are left alone.
        let (deactivated, _) = availability
            .apply_probe("rtx", &[], &universe, Some("connection refused"))
            .await;
        assert_eq!(deactivated, 2);
        assert!(availability.unavailable("rtx", "kimi").await.is_some());
        assert_eq!(
            availability.unavailable("rtx", "glm").await.unwrap().set_by,
            "operator"
        );
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    async fn persistence_round_trips() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("eavs").join("model-availability.json");
        {
            let availability = Availability::load(Some(path.clone()));
            availability
                .deactivate(
                    "rtx",
                    &models(["qwen3.8"]),
                    Some("why".into()),
                    None,
                    "operator",
                )
                .await;
        }
        let reloaded = Availability::load(Some(path));
        assert!(reloaded.unavailable("rtx", "qwen3.8").await.is_some());
        Ok(())
    }
}
