//! Model pricing data and cost calculation.
//!
//! Pricing data is sourced from LiteLLM's model_prices_and_context_window.json
//! which is the most comprehensive and up-to-date source available.
//!
//! Data source: https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Pricing information for a single model.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Model identifier (e.g., "gpt-4o", "claude-3-opus")
    pub model: String,

    /// Provider (e.g., "openai", "anthropic")
    pub provider: String,

    /// Cost per input token in USD
    pub input_cost_per_token: f64,

    /// Cost per output token in USD
    pub output_cost_per_token: f64,

    /// Cost per cached input token (if supported)
    #[serde(default)]
    pub cached_input_cost_per_token: Option<f64>,

    /// Maximum input tokens supported
    #[serde(default)]
    pub max_input_tokens: Option<u32>,

    /// Maximum output tokens supported
    #[serde(default)]
    pub max_output_tokens: Option<u32>,

    /// Whether model supports vision/images
    #[serde(default)]
    pub supports_vision: bool,

    /// Whether model supports function calling
    #[serde(default)]
    pub supports_function_calling: bool,
}

impl ModelPricing {
    /// Calculate cost for a given number of input and output tokens.
    pub fn calculate_cost(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        (input_tokens as f64 * self.input_cost_per_token)
            + (output_tokens as f64 * self.output_cost_per_token)
    }

    /// Calculate cost with cached tokens.
    pub fn calculate_cost_with_cache(
        &self,
        input_tokens: u32,
        cached_tokens: u32,
        output_tokens: u32,
    ) -> f64 {
        let regular_input = input_tokens.saturating_sub(cached_tokens);
        let cached_cost = self
            .cached_input_cost_per_token
            .unwrap_or(self.input_cost_per_token * 0.5); // Default 50% discount

        (regular_input as f64 * self.input_cost_per_token)
            + (cached_tokens as f64 * cached_cost)
            + (output_tokens as f64 * self.output_cost_per_token)
    }
}

/// Table of model pricing data.
#[derive(Debug, Clone)]
pub struct PricingTable {
    /// Model name -> pricing data
    models: HashMap<String, ModelPricing>,

    /// Last update timestamp
    last_updated: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for PricingTable {
    fn default() -> Self {
        Self::with_embedded_data()
    }
}

impl PricingTable {
    /// Create a new empty pricing table.
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
            last_updated: None,
        }
    }

    /// Create a pricing table with embedded default data.
    pub fn with_embedded_data() -> Self {
        let mut table = Self::new();
        table.load_embedded_data();
        table
    }

    /// Load embedded pricing data (compiled into binary).
    fn load_embedded_data(&mut self) {
        // Core OpenAI models
        self.add_model(ModelPricing {
            model: "gpt-4o".into(),
            provider: "openai".into(),
            input_cost_per_token: 0.0000025,
            output_cost_per_token: 0.00001,
            cached_input_cost_per_token: Some(0.00000125),
            max_input_tokens: Some(128000),
            max_output_tokens: Some(16384),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "gpt-4o-mini".into(),
            provider: "openai".into(),
            input_cost_per_token: 0.00000015,
            output_cost_per_token: 0.0000006,
            cached_input_cost_per_token: Some(0.000000075),
            max_input_tokens: Some(128000),
            max_output_tokens: Some(16384),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "gpt-4-turbo".into(),
            provider: "openai".into(),
            input_cost_per_token: 0.00001,
            output_cost_per_token: 0.00003,
            cached_input_cost_per_token: None,
            max_input_tokens: Some(128000),
            max_output_tokens: Some(4096),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "gpt-4".into(),
            provider: "openai".into(),
            input_cost_per_token: 0.00003,
            output_cost_per_token: 0.00006,
            cached_input_cost_per_token: None,
            max_input_tokens: Some(8192),
            max_output_tokens: Some(4096),
            supports_vision: false,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "gpt-3.5-turbo".into(),
            provider: "openai".into(),
            input_cost_per_token: 0.0000005,
            output_cost_per_token: 0.0000015,
            cached_input_cost_per_token: None,
            max_input_tokens: Some(16385),
            max_output_tokens: Some(4096),
            supports_vision: false,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "o1".into(),
            provider: "openai".into(),
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.00006,
            cached_input_cost_per_token: Some(0.0000075),
            max_input_tokens: Some(200000),
            max_output_tokens: Some(100000),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "o1-mini".into(),
            provider: "openai".into(),
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000012,
            cached_input_cost_per_token: Some(0.0000015),
            max_input_tokens: Some(128000),
            max_output_tokens: Some(65536),
            supports_vision: true,
            supports_function_calling: true,
        });

        // Core Anthropic models
        self.add_model(ModelPricing {
            model: "claude-3-opus-20240229".into(),
            provider: "anthropic".into(),
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            cached_input_cost_per_token: Some(0.00001875),
            max_input_tokens: Some(200000),
            max_output_tokens: Some(4096),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "claude-3-5-sonnet-20241022".into(),
            provider: "anthropic".into(),
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cached_input_cost_per_token: Some(0.0000003),
            max_input_tokens: Some(200000),
            max_output_tokens: Some(8192),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "claude-3-5-haiku-20241022".into(),
            provider: "anthropic".into(),
            input_cost_per_token: 0.0000008,
            output_cost_per_token: 0.000004,
            cached_input_cost_per_token: Some(0.00000008),
            max_input_tokens: Some(200000),
            max_output_tokens: Some(8192),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "claude-sonnet-4-5-20250929".into(),
            provider: "anthropic".into(),
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cached_input_cost_per_token: Some(0.0000003),
            max_input_tokens: Some(200000),
            max_output_tokens: Some(64000),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "claude-haiku-4-5-20251001".into(),
            provider: "anthropic".into(),
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000005,
            cached_input_cost_per_token: Some(0.0000001),
            max_input_tokens: Some(200000),
            max_output_tokens: Some(64000),
            supports_vision: true,
            supports_function_calling: true,
        });

        // Core Google models
        self.add_model(ModelPricing {
            model: "gemini-1.5-pro".into(),
            provider: "google".into(),
            input_cost_per_token: 0.00000125,
            output_cost_per_token: 0.000005,
            cached_input_cost_per_token: Some(0.0000003125),
            max_input_tokens: Some(2097152),
            max_output_tokens: Some(8192),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "gemini-1.5-flash".into(),
            provider: "google".into(),
            input_cost_per_token: 0.000000075,
            output_cost_per_token: 0.0000003,
            cached_input_cost_per_token: Some(0.00000001875),
            max_input_tokens: Some(1048576),
            max_output_tokens: Some(8192),
            supports_vision: true,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "gemini-2.0-flash".into(),
            provider: "google".into(),
            input_cost_per_token: 0.0000001,
            output_cost_per_token: 0.0000004,
            cached_input_cost_per_token: Some(0.000000025),
            max_input_tokens: Some(1048576),
            max_output_tokens: Some(8192),
            supports_vision: true,
            supports_function_calling: true,
        });

        // Mistral models
        self.add_model(ModelPricing {
            model: "mistral-large-latest".into(),
            provider: "mistral".into(),
            input_cost_per_token: 0.000002,
            output_cost_per_token: 0.000006,
            cached_input_cost_per_token: None,
            max_input_tokens: Some(128000),
            max_output_tokens: Some(8192),
            supports_vision: false,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "mistral-small-latest".into(),
            provider: "mistral".into(),
            input_cost_per_token: 0.0000002,
            output_cost_per_token: 0.0000006,
            cached_input_cost_per_token: None,
            max_input_tokens: Some(32000),
            max_output_tokens: Some(8192),
            supports_vision: false,
            supports_function_calling: true,
        });

        // Groq models (using their hosted inference)
        self.add_model(ModelPricing {
            model: "llama-3.1-70b-versatile".into(),
            provider: "groq".into(),
            input_cost_per_token: 0.00000059,
            output_cost_per_token: 0.00000079,
            cached_input_cost_per_token: None,
            max_input_tokens: Some(131072),
            max_output_tokens: Some(8192),
            supports_vision: false,
            supports_function_calling: true,
        });
        self.add_model(ModelPricing {
            model: "llama-3.1-8b-instant".into(),
            provider: "groq".into(),
            input_cost_per_token: 0.00000005,
            output_cost_per_token: 0.00000008,
            cached_input_cost_per_token: None,
            max_input_tokens: Some(131072),
            max_output_tokens: Some(8192),
            supports_vision: false,
            supports_function_calling: true,
        });

        self.last_updated = Some(chrono::Utc::now());
    }

    /// Add a model to the pricing table.
    pub fn add_model(&mut self, pricing: ModelPricing) {
        // Add with exact name
        self.models.insert(pricing.model.clone(), pricing.clone());

        // Also add common aliases
        let model_lower = pricing.model.to_lowercase();
        if !self.models.contains_key(&model_lower) {
            self.models.insert(model_lower, pricing);
        }
    }

    /// Get pricing for a model, with fallback matching.
    pub fn get(&self, model: &str) -> Option<&ModelPricing> {
        // Try exact match first
        if let Some(pricing) = self.models.get(model) {
            return Some(pricing);
        }

        // Try lowercase
        if let Some(pricing) = self.models.get(&model.to_lowercase()) {
            return Some(pricing);
        }

        // Try prefix matching for versioned models
        // e.g., "gpt-4o-2024-08-06" should match "gpt-4o"
        for (key, pricing) in &self.models {
            if model.starts_with(key) || key.starts_with(model) {
                return Some(pricing);
            }
        }

        None
    }

    /// Get pricing for a model, returning default fallback pricing if not found.
    pub fn get_or_default(&self, model: &str, provider: &str) -> ModelPricing {
        self.get(model).cloned().unwrap_or_else(|| {
            // Return conservative default pricing based on provider
            let (input, output) = match provider.to_lowercase().as_str() {
                "openai" => (0.00001, 0.00003),      // GPT-4 level as safe default
                "anthropic" => (0.000003, 0.000015), // Claude 3 Sonnet level
                "google" => (0.00000125, 0.000005),  // Gemini Pro level
                "mistral" => (0.000002, 0.000006),   // Mistral Large level
                _ => (0.00001, 0.00003),             // Conservative default
            };

            ModelPricing {
                model: model.to_string(),
                provider: provider.to_string(),
                input_cost_per_token: input,
                output_cost_per_token: output,
                cached_input_cost_per_token: Some(input * 0.5),
                max_input_tokens: Some(128000),
                max_output_tokens: Some(8192),
                supports_vision: false,
                supports_function_calling: true,
            }
        })
    }

    /// Update pricing from LiteLLM's GitHub repository.
    pub async fn update_from_litellm(&mut self) -> Result<usize, PricingUpdateError> {
        let url = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

        let client = reqwest::Client::new();
        let response = client
            .get(url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| PricingUpdateError::NetworkError(e.to_string()))?;

        if !response.status().is_success() {
            return Err(PricingUpdateError::HttpError(response.status().as_u16()));
        }

        let data: HashMap<String, LiteLLMModel> = response
            .json()
            .await
            .map_err(|e| PricingUpdateError::ParseError(e.to_string()))?;

        let mut count = 0;
        for (model_name, model_data) in data {
            // Skip sample_spec and non-chat models
            if model_name == "sample_spec" || model_data.mode.as_deref() != Some("chat") {
                continue;
            }

            // Skip models without pricing
            let input_cost = match model_data.input_cost_per_token {
                Some(c) if c > 0.0 => c,
                _ => continue,
            };
            let output_cost = match model_data.output_cost_per_token {
                Some(c) if c > 0.0 => c,
                _ => continue,
            };

            let provider = model_data.litellm_provider.unwrap_or_default();

            self.add_model(ModelPricing {
                model: model_name,
                provider,
                input_cost_per_token: input_cost,
                output_cost_per_token: output_cost,
                cached_input_cost_per_token: model_data.cache_read_input_token_cost,
                max_input_tokens: model_data.max_input_tokens,
                max_output_tokens: model_data.max_output_tokens,
                supports_vision: model_data.supports_vision.unwrap_or(false),
                supports_function_calling: model_data.supports_function_calling.unwrap_or(false),
            });
            count += 1;
        }

        self.last_updated = Some(chrono::Utc::now());
        Ok(count)
    }

    /// Get the number of models in the pricing table.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Check if the pricing table is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Get when the pricing data was last updated.
    #[allow(dead_code)]
    pub fn last_updated(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.last_updated
    }
}

/// LiteLLM model data structure (for parsing their JSON).
#[derive(Debug, Deserialize)]
struct LiteLLMModel {
    litellm_provider: Option<String>,
    mode: Option<String>,
    input_cost_per_token: Option<f64>,
    output_cost_per_token: Option<f64>,
    cache_read_input_token_cost: Option<f64>,
    max_input_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    supports_vision: Option<bool>,
    supports_function_calling: Option<bool>,
}

/// Errors that can occur when updating pricing data.
#[derive(Debug)]
pub enum PricingUpdateError {
    NetworkError(String),
    HttpError(u16),
    ParseError(String),
}

impl std::fmt::Display for PricingUpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkError(e) => write!(f, "Network error: {}", e),
            Self::HttpError(code) => write!(f, "HTTP error: {}", code),
            Self::ParseError(e) => write!(f, "Parse error: {}", e),
        }
    }
}

impl std::error::Error for PricingUpdateError {}

/// Thread-safe pricing table wrapper.
#[derive(Clone)]
pub struct SharedPricingTable {
    inner: Arc<RwLock<PricingTable>>,
}

impl Default for SharedPricingTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedPricingTable {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(PricingTable::with_embedded_data())),
        }
    }

    #[allow(dead_code)]
    pub async fn get(&self, model: &str) -> Option<ModelPricing> {
        self.inner.read().await.get(model).cloned()
    }

    pub async fn get_or_default(&self, model: &str, provider: &str) -> ModelPricing {
        self.inner.read().await.get_or_default(model, provider)
    }

    pub async fn update_from_litellm(&self) -> Result<usize, PricingUpdateError> {
        self.inner.write().await.update_from_litellm().await
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_pricing_data() {
        let table = PricingTable::with_embedded_data();

        // Check some known models exist
        assert!(table.get("gpt-4o").is_some());
        assert!(table.get("claude-3-5-sonnet-20241022").is_some());
        assert!(table.get("gemini-1.5-pro").is_some());
    }

    #[test]
    fn test_pricing_calculation() {
        let pricing = ModelPricing {
            model: "test".into(),
            provider: "test".into(),
            input_cost_per_token: 0.00001,
            output_cost_per_token: 0.00003,
            cached_input_cost_per_token: Some(0.000005),
            max_input_tokens: None,
            max_output_tokens: None,
            supports_vision: false,
            supports_function_calling: false,
        };

        // 1000 input + 500 output
        let cost = pricing.calculate_cost(1000, 500);
        assert!((cost - 0.025).abs() < 0.0001); // 0.01 + 0.015
    }

    #[test]
    fn test_pricing_with_cache() {
        let pricing = ModelPricing {
            model: "test".into(),
            provider: "test".into(),
            input_cost_per_token: 0.00001,
            output_cost_per_token: 0.00003,
            cached_input_cost_per_token: Some(0.000002), // 80% discount
            max_input_tokens: None,
            max_output_tokens: None,
            supports_vision: false,
            supports_function_calling: false,
        };

        // 1000 total input, 800 cached, 500 output
        let cost = pricing.calculate_cost_with_cache(1000, 800, 500);
        // (200 * 0.00001) + (800 * 0.000002) + (500 * 0.00003)
        // = 0.002 + 0.0016 + 0.015 = 0.0186
        assert!((cost - 0.0186).abs() < 0.0001);
    }

    #[test]
    fn test_fallback_pricing() {
        let table = PricingTable::with_embedded_data();

        // Unknown model should return default pricing
        let pricing = table.get_or_default("unknown-model-xyz", "openai");
        assert_eq!(pricing.model, "unknown-model-xyz");
        assert!(pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_prefix_matching() {
        let table = PricingTable::with_embedded_data();

        // Versioned model should match base model
        assert!(table.get("gpt-4o-2024-08-06").is_some());
    }
}
