//! Cost calculation and token counting.
//!
//! Uses tiktoken-rs for accurate token counting and the pricing table
//! for cost estimation.

use crate::keys::pricing::SharedPricingTable;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tiktoken_rs::{cl100k_base, o200k_base, CoreBPE};

/// Usage statistics for a request.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStats {
    /// Input tokens (prompt)
    pub input_tokens: u32,

    /// Output tokens (completion)
    pub output_tokens: u32,

    /// Cached input tokens (if applicable)
    pub cached_tokens: u32,

    /// Estimated cost in USD
    pub estimated_cost_usd: f64,

    /// Model used
    pub model: String,

    /// Provider used
    pub provider: String,
}

/// Calculator for estimating request costs.
pub struct CostCalculator {
    pricing: SharedPricingTable,
    /// Tokenizer for OpenAI models (cl100k_base for GPT-4, etc.)
    tokenizer_cl100k: Arc<CoreBPE>,
    /// Tokenizer for newer OpenAI models (o200k_base for GPT-4o, o1, etc.)
    tokenizer_o200k: Arc<CoreBPE>,
}

impl CostCalculator {
    /// Create a new cost calculator with the given pricing table.
    pub fn new(pricing: SharedPricingTable) -> Self {
        Self {
            pricing,
            tokenizer_cl100k: Arc::new(cl100k_base().expect("Failed to load cl100k tokenizer")),
            tokenizer_o200k: Arc::new(o200k_base().expect("Failed to load o200k tokenizer")),
        }
    }

    /// Get the appropriate tokenizer for a model.
    fn get_tokenizer(&self, model: &str) -> &CoreBPE {
        let model_lower = model.to_lowercase();

        // o200k_base is used for GPT-4o, o1, and newer models
        if model_lower.contains("gpt-4o")
            || model_lower.contains("o1")
            || model_lower.contains("o3")
            || model_lower.contains("chatgpt-4o")
        {
            &self.tokenizer_o200k
        } else {
            // cl100k_base is used for GPT-4, GPT-3.5-turbo, and most other models
            // It's also a reasonable approximation for Claude, Gemini, etc.
            &self.tokenizer_cl100k
        }
    }

    /// Count tokens in a string.
    pub fn count_tokens(&self, text: &str, model: &str) -> u32 {
        let tokenizer = self.get_tokenizer(model);
        tokenizer.encode_with_special_tokens(text).len() as u32
    }

    /// Count tokens in a chat message (approximation).
    ///
    /// This accounts for message framing overhead (role, separators, etc.)
    pub fn count_message_tokens(&self, role: &str, content: &str, model: &str) -> u32 {
        let tokenizer = self.get_tokenizer(model);

        // Base content tokens
        let content_tokens = tokenizer.encode_with_special_tokens(content).len() as u32;

        // Add overhead for message framing
        // OpenAI uses ~4 tokens per message for framing
        // This is an approximation that works across providers
        let framing_tokens = match role {
            "system" => 4,
            "user" => 4,
            "assistant" => 4,
            "tool" => 5, // Tool results have slightly more overhead
            _ => 4,
        };

        content_tokens + framing_tokens
    }

    /// Estimate tokens for a complete request body.
    pub fn estimate_request_tokens(&self, body: &serde_json::Value, model: &str) -> u32 {
        let mut total = 0;

        // Count system prompt
        if let Some(system) = body.get("system").and_then(|v| v.as_str()) {
            total += self.count_message_tokens("system", system, model);
        }

        // Count messages
        if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
            for msg in messages {
                let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");

                // Handle string content
                if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                    total += self.count_message_tokens(role, content, model);
                }
                // Handle array content (multimodal)
                else if let Some(content_array) = msg.get("content").and_then(|v| v.as_array()) {
                    for part in content_array {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            total += self.count_tokens(text, model);
                        }
                        // Images are counted separately (typically 85-170 tokens per tile)
                        if part.get("type").and_then(|v| v.as_str()) == Some("image_url") {
                            total += 170; // Conservative estimate per image
                        }
                    }
                    total += 4; // Message framing
                }
            }
        }

        // Add overhead for tools/functions if present
        if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
            for tool in tools {
                // Serialize tool definition and count tokens
                if let Ok(tool_str) = serde_json::to_string(tool) {
                    total += self.count_tokens(&tool_str, model);
                }
            }
        }

        total
    }

    /// Estimate cost for a request before it's sent.
    #[allow(dead_code)]
    pub async fn estimate_request_cost(
        &self,
        body: &serde_json::Value,
        model: &str,
        provider: &str,
    ) -> UsageStats {
        let input_tokens = self.estimate_request_tokens(body, model);

        // Estimate output tokens based on max_tokens or default
        let estimated_output = body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(1000); // Default estimate

        let pricing = self.pricing.get_or_default(model, provider).await;
        let cost = pricing.calculate_cost(input_tokens, estimated_output);

        UsageStats {
            input_tokens,
            output_tokens: estimated_output,
            cached_tokens: 0,
            estimated_cost_usd: cost,
            model: model.to_string(),
            provider: provider.to_string(),
        }
    }

    /// Calculate cost from token counts.
    pub async fn calculate_actual_cost(
        &self,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cached_tokens: u32,
    ) -> f64 {
        let pricing = self.pricing.get_or_default(model, "").await;
        if cached_tokens > 0 {
            pricing.calculate_cost_with_cache(input_tokens, cached_tokens, output_tokens)
        } else {
            pricing.calculate_cost(input_tokens, output_tokens)
        }
    }

    /// Calculate actual cost from API response usage data and return full stats.
    #[allow(dead_code)]
    pub async fn calculate_actual_cost_with_stats(
        &self,
        usage: &serde_json::Value,
        model: &str,
        provider: &str,
    ) -> UsageStats {
        // Extract token counts from response
        // Different providers use different field names
        let input_tokens = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(0);

        let output_tokens = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(0);

        let cached_tokens = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .or_else(|| usage.get("cache_read_input_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(0);

        let pricing = self.pricing.get_or_default(model, provider).await;
        let cost = if cached_tokens > 0 {
            pricing.calculate_cost_with_cache(input_tokens, cached_tokens, output_tokens)
        } else {
            pricing.calculate_cost(input_tokens, output_tokens)
        };

        UsageStats {
            input_tokens,
            output_tokens,
            cached_tokens,
            estimated_cost_usd: cost,
            model: model.to_string(),
            provider: provider.to_string(),
        }
    }

    /// Parse usage from a streaming response's final message.
    #[allow(dead_code)]
    pub fn parse_streaming_usage(&self, chunk: &serde_json::Value) -> Option<serde_json::Value> {
        // OpenAI format
        if let Some(usage) = chunk.get("usage") {
            if !usage.is_null() {
                return Some(usage.clone());
            }
        }

        // Anthropic format (in message_stop event)
        if let Some(usage) = chunk.get("message").and_then(|m| m.get("usage")) {
            return Some(usage.clone());
        }

        None
    }
}

impl Clone for CostCalculator {
    fn clone(&self) -> Self {
        Self {
            pricing: self.pricing.clone(),
            tokenizer_cl100k: self.tokenizer_cl100k.clone(),
            tokenizer_o200k: self.tokenizer_o200k.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_calculator() -> CostCalculator {
        CostCalculator::new(SharedPricingTable::new())
    }

    #[test]
    fn test_count_tokens() {
        let calc = make_calculator();

        let tokens = calc.count_tokens("Hello, world!", "gpt-4");
        assert!(tokens > 0);
        assert!(tokens < 10); // "Hello, world!" should be ~4 tokens
    }

    #[test]
    fn test_count_tokens_different_models() {
        let calc = make_calculator();

        let text = "The quick brown fox jumps over the lazy dog.";
        let tokens_gpt4 = calc.count_tokens(text, "gpt-4");
        let tokens_gpt4o = calc.count_tokens(text, "gpt-4o");

        // Both should give reasonable results
        assert!(tokens_gpt4 > 0);
        assert!(tokens_gpt4o > 0);
        // Token counts may differ slightly between tokenizers
    }

    #[test]
    fn test_estimate_request_tokens() {
        let calc = make_calculator();

        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "What is 2+2?"}
            ]
        });

        let tokens = calc.estimate_request_tokens(&body, "gpt-4");
        assert!(tokens > 10); // Should be more than just the text
        assert!(tokens < 50); // But not too many for this short conversation
    }

    #[test]
    fn test_estimate_request_with_tools() {
        let calc = make_calculator();

        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "What's the weather?"}
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "Get the current weather",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "location": {"type": "string"}
                            }
                        }
                    }
                }
            ]
        });

        let tokens_with_tools = calc.estimate_request_tokens(&body, "gpt-4");

        let body_no_tools = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "What's the weather?"}
            ]
        });
        let tokens_without_tools = calc.estimate_request_tokens(&body_no_tools, "gpt-4");

        // Should have more tokens with tools
        assert!(tokens_with_tools > tokens_without_tools);
    }

    #[tokio::test]
    async fn test_calculate_actual_cost() {
        let calc = make_calculator();

        // Test direct cost calculation
        let cost = calc.calculate_actual_cost("gpt-4o", 100, 50, 0).await;

        assert!(cost > 0.0);
    }

    #[tokio::test]
    async fn test_calculate_cost_with_cache() {
        let calc = make_calculator();

        // Test direct cost calculation with cache
        let cost_with_cache = calc.calculate_actual_cost("gpt-4o", 1000, 100, 800).await;
        let cost_without_cache = calc.calculate_actual_cost("gpt-4o", 1000, 100, 0).await;

        // Cost with cache should be lower
        assert!(cost_with_cache < cost_without_cache);
        assert!(cost_with_cache > 0.0);
    }

    #[tokio::test]
    async fn test_calculate_actual_cost_with_stats() {
        let calc = make_calculator();

        let usage = json!({
            "prompt_tokens": 1000,
            "completion_tokens": 100,
            "prompt_tokens_details": {
                "cached_tokens": 800
            }
        });

        let stats = calc
            .calculate_actual_cost_with_stats(&usage, "gpt-4o", "openai")
            .await;

        assert_eq!(stats.input_tokens, 1000);
        assert_eq!(stats.cached_tokens, 800);
        assert!(stats.estimated_cost_usd > 0.0);
    }
}
