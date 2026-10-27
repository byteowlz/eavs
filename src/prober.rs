//! Local-runtime availability prober.
//!
//! For providers with `[providers.*.probe] enabled`, periodically ask the
//! runtime for its served model list — a plain `GET /models`, the same call
//! any OpenAI-compatible client makes first. It never generates, so checking
//! costs no tokens and no quota. The result feeds the availability store:
//! configured models the runtime no longer lists are deactivated with
//! `set_by = "probe"` until the runtime serves them again.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::{debug, info};

use crate::availability::Availability;
use crate::config::ProviderConfig;
use crate::upstream::Upstream;

/// Where a runtime advertises its served models, relative to `base_url`.
const MODELS_PATH: &str = "/models";
/// Bound on the model-list response, plenty for a few thousand ids.
const MAX_MODELS_BYTES: usize = 512 * 1024;
/// Wake-up cadence of the scheduler loop.
const TICK: Duration = Duration::from_secs(5);

/// Model ids in a model-list response. Knows the OpenAI shape (`data[].id`),
/// which `llama.cpp`, `vLLM` and `llama-swap` speak, and `ollama`'s native
/// shape (`models[].name`).
pub fn parse_served_models(body: &[u8]) -> Option<Vec<String>> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let ids = |entries: &Value, key: &str| -> Vec<String> {
        entries
            .get(key)
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        entry
                            .get("id")
                            .or_else(|| entry.get("name"))
                            .or_else(|| entry.get("model"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut served = ids(&json, "data");
    if served.is_empty() {
        served = ids(&json, "models");
    }
    served.sort();
    served.dedup();
    Some(served)
}

/// One probe round against one runtime: ask for the model list, fold the
/// answer into the availability store.
pub async fn probe_once(
    upstream: &dyn Upstream,
    provider: &str,
    base_url: &str,
    probe: &crate::config::ProbeConfig,
    configured: &[String],
    availability: &Availability,
) -> (usize, usize) {
    let url = format!("{base_url}{MODELS_PATH}");
    let outcome = upstream
        .get_bytes(&url, probe.resolved_timeout(), MAX_MODELS_BYTES)
        .await;
    let served = match outcome {
        Ok((_, body)) if body.len() >= MAX_MODELS_BYTES => {
            Err(String::from("model list exceeded the response bound"))
        }
        Ok((_, body)) => parse_served_models(&body)
            .inspect(|served| debug!(provider, served = served.len(), "Runtime model list"))
            .ok_or_else(|| String::from("unrecognized model-list shape")),
        Err(error) => Err(error.to_string()),
    };
    let universe = universe(
        availability,
        provider,
        configured,
        served.as_deref().unwrap_or(&[]),
    )
    .await;
    match served {
        Ok(served) => {
            availability
                .apply_probe(provider, &served, &universe, None)
                .await
        }
        Err(error) => {
            availability
                .apply_probe(provider, &[], &universe, Some(&error))
                .await
        }
    }
}

/// Every model the runtime could serve: the configured shortlist when there
/// is one, otherwise the union of what previous probes saw and what this one
/// sees (a runtime that stops serving a model vanishes from its list, so it
/// must be remembered to be noticed missing).
async fn universe(
    availability: &Availability,
    provider: &str,
    configured: &[String],
    served: &[String],
) -> Vec<String> {
    if !configured.is_empty() {
        // A shortlist is the universe: a configured model the runtime does
        // not list is unavailable, even if no probe ever saw it served.
        return configured.to_vec();
    }
    let mut universe: std::collections::BTreeSet<String> = availability
        .last_served(provider)
        .await
        .into_iter()
        .collect();
    universe.extend(served.iter().cloned());
    universe.into_iter().collect()
}

/// Background scheduler: probes each enabled runtime on its own cadence.
/// Exits when the runtime shuts down with the process.
pub fn spawn(state: Arc<crate::state::AppState>) {
    tokio::spawn(async move {
        info!("Local-runtime availability prober started");
        let mut next_due: BTreeMap<String, Instant> = BTreeMap::new();
        loop {
            tokio::time::sleep(TICK).await;
            // File providers and providers added at runtime (console/admin API).
            let enabled: Vec<(String, crate::config::ProbeConfig, ProviderConfig)> = state
                .runtime_providers()
                .await
                .into_iter()
                .filter_map(|runtime| {
                    let probe = runtime.config.probe.clone().filter(|probe| probe.enabled)?;
                    Some((runtime.resolved_name, probe, runtime.config))
                })
                .collect();
            if enabled.is_empty() {
                continue;
            }
            for (name, probe, config) in enabled {
                let due = next_due.get(&name).is_none_or(|due| Instant::now() >= *due);
                if !due {
                    continue;
                }
                next_due.insert(name.clone(), Instant::now() + probe.resolved_interval());
                let configured: Vec<String> =
                    config.models.iter().map(|model| model.id.clone()).collect();
                let (deactivated, reactivated) = probe_once(
                    state.upstream.as_ref(),
                    &name,
                    &config.resolved_base_url(),
                    &probe,
                    &configured,
                    &state.availability,
                )
                .await;
                if deactivated != 0 || reactivated != 0 {
                    info!(
                        provider = %name,
                        deactivated,
                        reactivated,
                        "Availability probe updated local models"
                    );
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(clippy::unwrap_used, reason = "parse results are asserted directly")]
    fn parses_openai_and_ollama_model_lists() {
        let openai = br#"{"object":"list","data":[{"id":"qwen3.8"},{"id":"kimi-k3"}]}"#;
        assert_eq!(
            parse_served_models(openai).unwrap(),
            vec!["kimi-k3".to_owned(), "qwen3.8".to_owned()]
        );
        let ollama = br#"{"models":[{"name":"qwen3.8:latest"},{"name":"kimi:32b"}]}"#;
        assert_eq!(
            parse_served_models(ollama).unwrap(),
            vec!["kimi:32b".to_owned(), "qwen3.8:latest".to_owned()]
        );
        assert_eq!(parse_served_models(b"<html>not json</html>"), None);
    }

    #[tokio::test]
    async fn shortlist_is_the_probe_universe() {
        let availability = Availability::load(None);
        let configured = vec!["qwen3.8".to_owned(), "kimi-k3".to_owned()];
        // The runtime serves only qwen3.8 (and something unconfigured).
        let served = vec!["qwen3.8".to_owned(), "other".to_owned()];
        let universe = universe(&availability, "rtx", &configured, &served).await;
        assert_eq!(universe, configured);
        availability
            .apply_probe("rtx", &served, &universe, None)
            .await;
        assert!(availability.unavailable("rtx", "kimi-k3").await.is_some());
        assert!(availability.unavailable("rtx", "qwen3.8").await.is_none());
        assert!(availability.unavailable("rtx", "other").await.is_none());
    }
}
