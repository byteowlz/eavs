use crate::config::{AppConfig, ModelShortlistEntry, ProviderConfig};
use crate::model_catalog::{eavs_to_catalog_id, ModelCatalog};

#[derive(Debug, Clone)]
pub struct ResolvedModelDefaults {
    pub provider: String,
    pub default: String,
    pub fast: String,
    pub reasoning: String,
    pub fallback: String,
}

pub fn resolve_model_defaults(
    config: &AppConfig,
    catalog: Option<&ModelCatalog>,
    explicit_provider: Option<&str>,
    runtime_default_provider: Option<&str>,
) -> Option<ResolvedModelDefaults> {
    let requested = explicit_provider
        .filter(|p| !p.is_empty())
        .or(runtime_default_provider)
        .unwrap_or("default");

    let provider_lookup =
        config
            .resolve_provider(requested)
            .or_else(|| config.resolve_provider("default"))
            .or_else(|| {
                config.providers.iter().next().map(|(name, cfg)| {
                    crate::config::ProviderLookupResult {
                        config: cfg,
                        resolved_name: name.clone(),
                        was_fallback: true,
                    }
                })
            })?;

    let provider_name = provider_lookup.resolved_name;
    let provider_config = provider_lookup.config;

    let models = candidate_models(provider_name.as_str(), provider_config, catalog);
    let default = choose_default_model(&models)?;
    let fast = choose_fast_model(&models, &default);
    let reasoning = choose_reasoning_model(&models, &default);

    Some(ResolvedModelDefaults {
        provider: provider_name,
        default: default.clone(),
        fast,
        reasoning,
        fallback: default,
    })
}

fn candidate_models(
    provider_name: &str,
    provider_config: &ProviderConfig,
    catalog: Option<&ModelCatalog>,
) -> Vec<ModelShortlistEntry> {
    if !provider_config.models.is_empty() {
        return provider_config.models.clone();
    }

    if !provider_config.test_model.is_empty() {
        return vec![ModelShortlistEntry {
            id: provider_config.test_model.clone(),
            name: provider_config.test_model.clone(),
            reasoning: false,
            input: vec!["text".to_string()],
            context_window: 128_000,
            max_tokens: 16_384,
            cost: crate::config::ModelCost::default(),
            compat: std::collections::HashMap::new(),
        }];
    }

    if let Some(catalog) = catalog {
        let catalog_id = eavs_to_catalog_id(provider_name, &provider_config.type_);
        return catalog.models_for_provider(catalog_id, &[]);
    }

    Vec::new()
}

fn choose_default_model(models: &[ModelShortlistEntry]) -> Option<String> {
    models
        .iter()
        .find(|m| !m.reasoning)
        .or_else(|| models.first())
        .map(|m| m.id.clone())
}

fn choose_fast_model(models: &[ModelShortlistEntry], default: &str) -> String {
    const FAST_HINTS: &[&str] = &[
        "mini", "haiku", "flash", "fast", "small", "nano", "8b", "turbo",
    ];

    models
        .iter()
        .find(|m| {
            let id = m.id.to_ascii_lowercase();
            FAST_HINTS.iter().any(|hint| id.contains(hint))
        })
        .or_else(|| models.iter().find(|m| !m.reasoning))
        .or_else(|| models.first())
        .map(|m| m.id.clone())
        .unwrap_or_else(|| default.to_string())
}

fn choose_reasoning_model(models: &[ModelShortlistEntry], default: &str) -> String {
    const REASONING_HINTS: &[&str] = &["reason", "thinking", "o1", "o3", "o4", "r1"];

    models
        .iter()
        .find(|m| m.reasoning)
        .or_else(|| {
            models.iter().find(|m| {
                let id = m.id.to_ascii_lowercase();
                REASONING_HINTS.iter().any(|hint| id.contains(hint))
            })
        })
        .or_else(|| models.first())
        .map(|m| m.id.clone())
        .unwrap_or_else(|| default.to_string())
}
