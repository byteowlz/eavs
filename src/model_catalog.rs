//! Model catalog backed by models.dev
//!
//! Fetches the full model catalog from https://models.dev/api.json and caches it locally.
//! Used to enrich provider detail responses and power the `eavs models` CLI.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

use crate::config::ModelShortlistEntry;

const CATALOG_URL: &str = "https://models.dev/api.json";
/// Re-fetch if cache is older than this
const CACHE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60); // 24h

/// A provider entry from models.dev
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogProvider {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub models: HashMap<String, CatalogModel>,
}

/// A model entry from models.dev
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogModel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default)]
    pub modalities: CatalogModalities,
    #[serde(default)]
    pub cost: CatalogCost,
    #[serde(default)]
    pub limit: CatalogLimit,
    #[serde(default)]
    pub knowledge: String,
    #[serde(default)]
    pub release_date: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CatalogModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CatalogCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CatalogLimit {
    #[serde(default)]
    pub context: u64,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
}

impl CatalogModel {
    /// Convert to our internal ModelShortlistEntry format.
    pub fn to_shortlist_entry(&self) -> ModelShortlistEntry {
        let input = if self.modalities.input.is_empty() {
            vec!["text".to_string()]
        } else {
            // Filter to just text/image (Pi's supported modalities)
            self.modalities
                .input
                .iter()
                .filter(|m| *m == "text" || *m == "image")
                .cloned()
                .collect()
        };

        ModelShortlistEntry {
            id: self.id.clone(),
            name: if self.name.is_empty() {
                self.id.clone()
            } else {
                self.name.clone()
            },
            reasoning: self.reasoning,
            input,
            context_window: if self.limit.context > 0 {
                self.limit.context
            } else {
                128_000
            },
            max_tokens: if self.limit.output > 0 {
                self.limit.output
            } else {
                16_384
            },
            cost: crate::config::ModelCost {
                input: self.cost.input,
                output: self.cost.output,
                cache_read: self.cost.cache_read,
            },
        }
    }
}

/// The in-memory catalog, keyed by provider ID.
pub struct ModelCatalog {
    providers: HashMap<String, CatalogProvider>,
}

impl ModelCatalog {
    /// Load catalog from cache or fetch fresh.
    pub async fn load() -> Result<Self> {
        let cache_path = cache_file_path();

        // Try cache first
        if let Some(cached) = try_load_cache(&cache_path) {
            debug!(
                "Loaded model catalog from cache ({} providers)",
                cached.len()
            );
            return Ok(Self { providers: cached });
        }

        // Fetch fresh
        match fetch_catalog().await {
            Ok(providers) => {
                info!(
                    "Fetched model catalog: {} providers",
                    providers.len()
                );
                // Write cache (best-effort)
                if let Err(e) = write_cache(&cache_path, &providers) {
                    warn!("Failed to write catalog cache: {}", e);
                }
                Ok(Self { providers })
            }
            Err(e) => {
                warn!("Failed to fetch model catalog: {}", e);
                // Return empty catalog rather than failing
                Ok(Self {
                    providers: HashMap::new(),
                })
            }
        }
    }

    /// Load only from cache (no network). Returns empty if no cache.
    pub fn load_cached_only() -> Self {
        let cache_path = cache_file_path();
        let providers = try_load_cache_any_age(&cache_path).unwrap_or_default();
        if !providers.is_empty() {
            debug!(
                "Loaded model catalog from cache ({} providers)",
                providers.len()
            );
        }
        Self { providers }
    }

    /// Force fetch and update cache.
    pub async fn refresh() -> Result<Self> {
        let providers = fetch_catalog().await?;
        let cache_path = cache_file_path();
        write_cache(&cache_path, &providers)?;
        info!(
            "Refreshed model catalog: {} providers, {} total models",
            providers.len(),
            providers.values().map(|p| p.models.len()).sum::<usize>()
        );
        Ok(Self { providers })
    }

    /// Get models for a provider.
    ///
    /// - Config shortlist non-empty: return ONLY those models (curated, locked down)
    /// - Config shortlist empty: return full catalog for this provider
    pub fn models_for_provider(
        &self,
        provider_id: &str,
        config_models: &[ModelShortlistEntry],
    ) -> Vec<ModelShortlistEntry> {
        // Non-empty config shortlist = authoritative, no catalog merge
        if !config_models.is_empty() {
            return config_models.to_vec();
        }

        // Empty shortlist = serve full catalog
        if let Some(provider) = self.providers.get(provider_id) {
            let mut catalog_models: Vec<&CatalogModel> = provider.models.values().collect();
            catalog_models.sort_by(|a, b| b.release_date.cmp(&a.release_date));
            catalog_models
                .into_iter()
                .map(|m| m.to_shortlist_entry())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get all models from the catalog for a provider (no config merge).
    pub fn catalog_models(&self, provider_id: &str) -> Vec<&CatalogModel> {
        self.providers
            .get(provider_id)
            .map(|p| {
                let mut models: Vec<&CatalogModel> = p.models.values().collect();
                models.sort_by(|a, b| b.release_date.cmp(&a.release_date));
                models
            })
            .unwrap_or_default()
    }

    /// List all provider IDs in the catalog.
    pub fn provider_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.providers.keys().map(|s| s.as_str()).collect();
        ids.sort();
        ids
    }

    /// Total number of models across all providers.
    pub fn total_models(&self) -> usize {
        self.providers.values().map(|p| p.models.len()).sum()
    }

    /// Is the catalog empty (e.g., fetch failed)?
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

/// Map from eavs provider name to models.dev provider ID.
///
/// Most map directly, but some need translation.
pub fn eavs_to_catalog_id<'a>(eavs_name: &'a str, eavs_type: &str) -> &'a str {
    match eavs_type {
        "openai" | "openai-responses" | "openai-codex" => "openai",
        "anthropic" => "anthropic",
        "google" | "google-vertex" | "google-gemini-cli" | "google-antigravity" => "google",
        "mistral" => "mistral",
        "groq" => "groq",
        "xai" => "xai",
        "openrouter" => "openrouter",
        "azure" => "azure",
        "bedrock" | "aws-bedrock" => "bedrock",
        "cerebras" => "cerebras",
        "github-copilot" => "github-copilot",
        // For unknown types, try the eavs provider name directly
        _ => eavs_name,
    }
}

// --- Cache helpers ---

fn cache_file_path() -> PathBuf {
    let data_dir = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".local/share")
        });
    data_dir.join("eavs").join("models_catalog.json")
}

fn try_load_cache(path: &PathBuf) -> Option<HashMap<String, CatalogProvider>> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;

    if age > CACHE_MAX_AGE {
        debug!("Catalog cache expired ({:.1}h old)", age.as_secs_f64() / 3600.0);
        return None;
    }

    try_load_cache_any_age(path)
}

fn try_load_cache_any_age(path: &PathBuf) -> Option<HashMap<String, CatalogProvider>> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_cache(path: &PathBuf, providers: &HashMap<String, CatalogProvider>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create cache directory")?;
    }
    let data = serde_json::to_string(providers).context("Failed to serialize catalog")?;
    std::fs::write(path, data).context("Failed to write catalog cache")?;
    Ok(())
}

async fn fetch_catalog() -> Result<HashMap<String, CatalogProvider>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    let resp = client
        .get(CATALOG_URL)
        .send()
        .await
        .context("Failed to fetch models.dev catalog")?;

    if !resp.status().is_success() {
        anyhow::bail!("models.dev returned {}", resp.status());
    }

    let providers: HashMap<String, CatalogProvider> = resp
        .json()
        .await
        .context("Failed to parse models.dev catalog")?;

    Ok(providers)
}
