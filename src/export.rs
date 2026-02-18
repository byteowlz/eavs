//! Export configured providers and models to agent harness formats.
//!
//! Each harness (Pi, OpenCode, etc.) expects a different JSON format for
//! model configuration. This module reads the eavs config + model catalog
//! and produces the correct output.

use serde_json::Value;

use crate::api::ProviderDetail;

/// Generate Pi-compatible models.json from provider details.
///
/// Output format:
/// ```json
/// {
///   "providers": {
///     "eavs-anthropic": {
///       "baseUrl": "http://127.0.0.1:3033/anthropic/v1",
///       "api": "anthropic-messages",
///       "apiKey": "EAVS_API_KEY",
///       "models": [
///         {
///           "id": "claude-sonnet-4-6",
///           "name": "Claude Sonnet 4.6",
///           "reasoning": true,
///           "input": ["text", "image"],
///           "contextWindow": 200000,
///           "maxTokens": 64000,
///           "cost": { "input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 0 }
///         }
///       ]
///     }
///   }
/// }
/// ```
pub fn to_pi(
    providers: &[ProviderDetail],
    eavs_base_url: &str,
    api_key: &str,
) -> Value {
    let mut pi_providers = serde_json::Map::new();
    let base = eavs_base_url.trim_end_matches('/');

    for provider in providers {
        let pi_api = match provider.pi_api.as_deref() {
            Some(a) => a,
            None => continue,
        };

        // Skip "default" provider alias
        if provider.name == "default" {
            continue;
        }

        let base_url = format!("{}/{}/v1", base, provider.name);

        let models: Vec<Value> = provider
            .models
            .iter()
            .map(|m| {
                let cost_obj = serde_json::json!({
                    "input": m.cost.input,
                    "output": m.cost.output,
                    "cacheRead": m.cost.cache_read,
                    "cacheWrite": 0
                });

                let input = if m.input.is_empty() {
                    vec!["text".to_string()]
                } else {
                    m.input.clone()
                };

                let name = if m.name.is_empty() { &m.id } else { &m.name };

                serde_json::json!({
                    "id": m.id,
                    "name": name,
                    "reasoning": m.reasoning,
                    "input": input,
                    "contextWindow": m.context_window,
                    "maxTokens": m.max_tokens,
                    "cost": cost_obj
                })
            })
            .collect();

        let pi_provider = serde_json::json!({
            "baseUrl": base_url,
            "api": pi_api,
            "apiKey": api_key,
            "models": models,
        });

        let pi_name = format!("eavs-{}", provider.name);
        pi_providers.insert(pi_name, pi_provider);
    }

    serde_json::json!({
        "providers": pi_providers,
    })
}

// Future: pub fn to_opencode(...) -> Value { ... }
