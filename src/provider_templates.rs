//! Provider templates for quick provider setup.
//!
//! Templates are derived from two sources:
//! 1. A shipped baseline TOML file with curated provider defaults
//! 2. Auto-generated entries from the models.dev catalog
//!
//! The baseline takes precedence — it can override auto-generated fields
//! for providers that need special handling (e.g., Bedrock with SigV4 auth,
//! GitHub Copilot with device code flow, OAuth providers).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

use std::path::PathBuf;

use crate::model_catalog::ModelCatalog;

/// Compiled-in fallback (always available).
const EMBEDDED_BASELINE: &str = include_str!("../config/provider-templates.toml");

/// A provider template with everything needed to configure a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderTemplate {
    /// Provider ID (e.g., "anthropic", "minimax", "groq")
    pub id: String,
    /// Human-readable display name
    pub name: String,
    /// Eavs provider type (e.g., "openai", "anthropic", "openai-compatible")
    #[serde(rename = "type")]
    pub type_: String,
    /// Default base URL (may be empty if user must supply it, e.g. Azure)
    #[serde(default)]
    pub base_url: Option<String>,
    /// Environment variable names for the API key
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// Authentication method hint
    #[serde(default)]
    pub auth: AuthHint,
    /// Documentation URL
    #[serde(default)]
    pub doc_url: Option<String>,
    /// Number of models available from this provider
    #[serde(default)]
    pub model_count: usize,
    /// Source: "baseline" (curated), "catalog" (auto from models.dev), or "merged"
    #[serde(default = "default_source")]
    pub source: String,
    /// Special notes for the user (e.g., "Requires OAuth login via `eavs login`")
    #[serde(default)]
    pub notes: Option<String>,
    /// Additional fields needed beyond api_key (e.g., aws_region for Bedrock)
    #[serde(default)]
    pub extra_fields: Vec<ExtraField>,
}

/// Additional config field required by a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraField {
    pub name: String,
    pub description: String,
    /// Suggested env var (e.g., "env:AWS_REGION")
    #[serde(default)]
    pub env_hint: Option<String>,
    pub required: bool,
}

/// How this provider authenticates.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AuthHint {
    /// Standard API key (Bearer or x-api-key)
    #[default]
    ApiKey,
    /// OAuth device code flow (e.g., GitHub Copilot)
    #[serde(alias = "oauth")]
    OAuth,
    /// AWS SigV4 signing
    AwsSigv4,
    /// Azure api-key header
    AzureApiKey,
    /// No authentication needed (e.g., local Ollama)
    None,
}

fn default_source() -> String {
    "baseline".to_string()
}

/// Baseline template entry as defined in the shipped TOML.
#[derive(Debug, Clone, Deserialize)]
struct BaselineEntry {
    name: Option<String>,
    #[serde(rename = "type")]
    type_: Option<String>,
    base_url: Option<String>,
    env_keys: Option<Vec<String>>,
    auth: Option<AuthHint>,
    doc_url: Option<String>,
    notes: Option<String>,
    #[serde(default)]
    extra_fields: Vec<ExtraField>,
    /// If true, this provider is hidden from auto-generation (fully manual)
    #[serde(default)]
    manual_only: bool,
}

/// Map npm package name to eavs provider type.
fn npm_to_eavs_type(npm: &str) -> Option<&'static str> {
    match npm {
        "@ai-sdk/openai" => Some("openai"),
        "@ai-sdk/anthropic" => Some("anthropic"),
        "@ai-sdk/google" => Some("google"),
        "@ai-sdk/google-vertex" | "@ai-sdk/google-vertex/anthropic" => Some("google-vertex"),
        "@ai-sdk/mistral" => Some("mistral"),
        "@ai-sdk/groq" => Some("groq"),
        "@ai-sdk/cerebras" => Some("cerebras"),
        "@ai-sdk/xai" => Some("xai"),
        "@ai-sdk/amazon-bedrock" => Some("bedrock"),
        "@ai-sdk/azure" => Some("azure"),
        "@ai-sdk/openai-compatible"
        | "@ai-sdk/togetherai"
        | "@ai-sdk/deepinfra"
        | "@ai-sdk/perplexity"
        | "@ai-sdk/cohere"
        | "@ai-sdk/gateway"
        | "@ai-sdk/vercel" => Some("openai-compatible"),
        "@openrouter/ai-sdk-provider" => Some("openrouter"),
        _ => Some("openai-compatible"), // safe fallback for unknown npm packages
    }
}

/// Search paths for the external provider-templates.toml file.
///
/// Precedence (first found wins):
/// 1. `~/.config/eavs/provider-templates.toml`  (user override)
/// 2. `/usr/share/eavs/provider-templates.toml`  (system, e.g., AUR)
/// 3. `$HOMEBREW_PREFIX/share/eavs/provider-templates.toml`
/// 4. `$XDG_DATA_HOME/eavs/provider-templates.toml` (cargo install / just install)
/// 5. Compiled-in fallback
fn external_template_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // User override (config dir).
    if let Ok(config_dir) = crate::paths::config_dir() {
        paths.push(config_dir.join("eavs").join("provider-templates.toml"));
    }

    // Install-location discovery: alongside the executable's own dir first.
    // This is portable (works on any OS, incl. relocatable / portable installs).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            // e.g. <prefix>/bin/eavs -> <prefix>/share/eavs/...
            if let Some(prefix) = exe_dir.parent() {
                paths.push(
                    prefix
                        .join("share")
                        .join("eavs")
                        .join("provider-templates.toml"),
                );
            }
            // Also check right next to the binary (portable layout).
            paths.push(exe_dir.join("provider-templates.toml"));
        }
    }

    // Unix system install prefixes (AUR, dpkg, Homebrew, etc.).
    #[cfg(unix)]
    {
        paths.push(PathBuf::from("/usr/share/eavs/provider-templates.toml"));

        if let Ok(prefix) = std::env::var("HOMEBREW_PREFIX") {
            paths.push(PathBuf::from(prefix).join("share/eavs/provider-templates.toml"));
        }
        paths.push(PathBuf::from(
            "/opt/homebrew/share/eavs/provider-templates.toml",
        ));
        paths.push(PathBuf::from(
            "/usr/local/share/eavs/provider-templates.toml",
        ));
    }

    // XDG data dir (cargo install / just install).
    if let Ok(data_dir) = crate::paths::data_dir() {
        paths.push(data_dir.join("eavs").join("provider-templates.toml"));
    }

    paths
}

/// Load baseline templates from external file or compiled-in fallback.
fn load_baseline() -> HashMap<String, BaselineEntry> {
    // Try external files first
    for path in external_template_paths() {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            match toml::from_str::<HashMap<String, BaselineEntry>>(&contents) {
                Ok(entries) => {
                    debug!(
                        "Loaded {} baseline provider templates from {}",
                        entries.len(),
                        path.display()
                    );
                    return entries;
                }
                Err(e) => {
                    warn!("Failed to parse {}: {}", path.display(), e);
                    // Continue to next path / fallback
                }
            }
        }
    }

    // Compiled-in fallback
    match toml::from_str::<HashMap<String, BaselineEntry>>(EMBEDDED_BASELINE) {
        Ok(entries) => {
            debug!(
                "Loaded {} baseline provider templates (compiled-in fallback)",
                entries.len()
            );
            entries
        }
        Err(e) => {
            warn!("Failed to parse compiled-in provider-templates.toml: {}", e);
            HashMap::new()
        }
    }
}

/// Build the full template list by merging baseline + catalog.
pub fn build_templates(catalog: Option<&ModelCatalog>) -> Vec<ProviderTemplate> {
    let baseline = load_baseline();
    let mut templates: HashMap<String, ProviderTemplate> = HashMap::new();

    // 1. Auto-generate from models.dev catalog
    if let Some(cat) = catalog {
        for (id, provider) in cat.all_providers() {
            // Skip providers with no models
            if provider.models.is_empty() {
                continue;
            }

            // Determine eavs type from npm field
            let eavs_type = provider
                .npm
                .as_deref()
                .and_then(npm_to_eavs_type)
                .unwrap_or("openai-compatible");

            let base_url = provider.api.clone().map(|url| {
                // Strip trailing /v1 etc. — eavs appends the path
                url.trim_end_matches("/v1")
                    .trim_end_matches("/v1/")
                    .to_string()
            });

            templates.insert(
                id.clone(),
                ProviderTemplate {
                    id: id.clone(),
                    name: if provider.name.is_empty() {
                        id.clone()
                    } else {
                        provider.name.clone()
                    },
                    type_: eavs_type.to_string(),
                    base_url,
                    env_keys: provider.env.clone(),
                    auth: AuthHint::ApiKey,
                    doc_url: provider.doc.clone(),
                    model_count: provider.models.len(),
                    source: "catalog".to_string(),
                    notes: None,
                    extra_fields: vec![],
                },
            );
        }
    }

    // 2. Overlay baseline entries (baseline wins on conflict)
    for (id, entry) in &baseline {
        if entry.manual_only {
            // Only include if there's a baseline, never auto-generate
            let template = ProviderTemplate {
                id: id.clone(),
                name: entry.name.clone().unwrap_or_else(|| id.clone()),
                type_: entry
                    .type_
                    .clone()
                    .unwrap_or_else(|| "openai-compatible".to_string()),
                base_url: entry.base_url.clone(),
                env_keys: entry.env_keys.clone().unwrap_or_default(),
                auth: entry.auth.clone().unwrap_or_default(),
                doc_url: entry.doc_url.clone(),
                model_count: catalog
                    .and_then(|c| c.get_provider(id))
                    .map(|p| p.models.len())
                    .unwrap_or(0),
                source: "baseline".to_string(),
                notes: entry.notes.clone(),
                extra_fields: entry.extra_fields.clone(),
            };
            templates.insert(id.clone(), template);
            continue;
        }

        if let Some(existing) = templates.get_mut(id) {
            // Merge: baseline overrides catalog fields when present
            if let Some(ref name) = entry.name {
                existing.name = name.clone();
            }
            if let Some(ref type_) = entry.type_ {
                existing.type_ = type_.clone();
            }
            if let Some(ref base_url) = entry.base_url {
                existing.base_url = Some(base_url.clone());
            }
            if let Some(ref env_keys) = entry.env_keys {
                existing.env_keys = env_keys.clone();
            }
            if let Some(ref auth) = entry.auth {
                existing.auth = auth.clone();
            }
            if let Some(ref doc_url) = entry.doc_url {
                existing.doc_url = Some(doc_url.clone());
            }
            if let Some(ref notes) = entry.notes {
                existing.notes = Some(notes.clone());
            }
            if !entry.extra_fields.is_empty() {
                existing.extra_fields = entry.extra_fields.clone();
            }
            existing.source = "merged".to_string();
        } else {
            // Baseline-only provider (not in catalog)
            templates.insert(
                id.clone(),
                ProviderTemplate {
                    id: id.clone(),
                    name: entry.name.clone().unwrap_or_else(|| id.clone()),
                    type_: entry
                        .type_
                        .clone()
                        .unwrap_or_else(|| "openai-compatible".to_string()),
                    base_url: entry.base_url.clone(),
                    env_keys: entry.env_keys.clone().unwrap_or_default(),
                    auth: entry.auth.clone().unwrap_or_default(),
                    doc_url: entry.doc_url.clone(),
                    model_count: 0,
                    source: "baseline".to_string(),
                    notes: entry.notes.clone(),
                    extra_fields: entry.extra_fields.clone(),
                },
            );
        }
    }

    let mut result: Vec<ProviderTemplate> = templates.into_values().collect();
    result.sort_by(|a, b| a.id.cmp(&b.id));
    result
}

/// Generate a ProviderConfig JSON blob from a template + user-supplied values.
pub fn template_to_config(template: &ProviderTemplate, api_key: Option<&str>) -> serde_json::Value {
    let mut config = serde_json::Map::new();
    config.insert("type".to_string(), serde_json::json!(template.type_));

    // API key: use provided value, or default to env: syntax
    if let Some(key) = api_key {
        config.insert("api_key".to_string(), serde_json::json!(key));
    } else if let Some(env_var) = template.env_keys.first() {
        config.insert(
            "api_key".to_string(),
            serde_json::json!(format!("env:{}", env_var)),
        );
    }

    // Base URL (only if template has one; otherwise eavs uses built-in defaults)
    if let Some(ref base_url) = template.base_url {
        config.insert("base_url".to_string(), serde_json::json!(base_url));
    }

    serde_json::Value::Object(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_baseline() {
        let baseline = load_baseline();
        assert!(baseline.contains_key("openai"), "should have openai");
        assert!(baseline.contains_key("anthropic"), "should have anthropic");
        assert!(
            baseline.contains_key("amazon-bedrock"),
            "should have bedrock"
        );
        assert!(baseline.contains_key("ollama"), "should have ollama");
        assert!(
            baseline.len() >= 15,
            "should have at least 15 baseline templates"
        );
    }

    #[test]
    fn test_build_templates_baseline_only() {
        let templates = build_templates(None);
        assert!(!templates.is_empty(), "should have templates from baseline");

        let openai = templates
            .iter()
            .find(|t| t.id == "openai")
            .expect("openai template");
        assert_eq!(openai.type_, "openai");
        assert_eq!(
            openai.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert!(openai.env_keys.contains(&"OPENAI_API_KEY".to_string()));

        let bedrock = templates
            .iter()
            .find(|t| t.id == "amazon-bedrock")
            .expect("bedrock template");
        assert_eq!(bedrock.auth, AuthHint::AwsSigv4);
        assert!(!bedrock.extra_fields.is_empty());
    }

    #[test]
    fn test_npm_to_eavs_type_mapping() {
        assert_eq!(npm_to_eavs_type("@ai-sdk/anthropic"), Some("anthropic"));
        assert_eq!(npm_to_eavs_type("@ai-sdk/openai"), Some("openai"));
        assert_eq!(
            npm_to_eavs_type("@ai-sdk/openai-compatible"),
            Some("openai-compatible")
        );
        assert_eq!(npm_to_eavs_type("@ai-sdk/google"), Some("google"));
        assert_eq!(npm_to_eavs_type("@ai-sdk/amazon-bedrock"), Some("bedrock"));
        assert_eq!(
            npm_to_eavs_type("@openrouter/ai-sdk-provider"),
            Some("openrouter")
        );
        // Unknown packages get openai-compatible fallback
        assert_eq!(
            npm_to_eavs_type("some-unknown-package"),
            Some("openai-compatible")
        );
    }

    #[test]
    fn test_template_to_config() {
        let template = ProviderTemplate {
            id: "test".to_string(),
            name: "Test".to_string(),
            type_: "anthropic".to_string(),
            base_url: Some("https://api.test.com/v1".to_string()),
            env_keys: vec!["TEST_API_KEY".to_string()],
            auth: AuthHint::ApiKey,
            doc_url: None,
            model_count: 5,
            source: "baseline".to_string(),
            notes: None,
            extra_fields: vec![],
        };

        // With explicit key
        let config = template_to_config(&template, Some("sk-123"));
        assert_eq!(config["type"], "anthropic");
        assert_eq!(config["api_key"], "sk-123");
        assert_eq!(config["base_url"], "https://api.test.com/v1");

        // Without key — falls back to env: syntax
        let config = template_to_config(&template, None);
        assert_eq!(config["api_key"], "env:TEST_API_KEY");
    }

    #[test]
    fn test_manual_only_not_auto_generated() {
        // Without catalog, manual_only providers should still appear from baseline
        let templates = build_templates(None);
        let bedrock = templates.iter().find(|t| t.id == "amazon-bedrock");
        assert!(
            bedrock.is_some(),
            "manual_only providers should appear from baseline"
        );
    }

    #[test]
    fn test_templates_sorted() {
        let templates = build_templates(None);
        for i in 1..templates.len() {
            assert!(
                templates[i - 1].id <= templates[i].id,
                "templates should be sorted by id"
            );
        }
    }
}
