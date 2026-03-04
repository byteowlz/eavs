//! Discover models from provider endpoints.
//!
//! Probes configured provider endpoints to discover available models,
//! supplementing the static models.dev catalog with dynamically discovered
//! models from local providers (Ollama, vLLM), cloud APIs, etc.

use crate::config::ProviderConfig;
use crate::provider::ProviderType;
use serde::Deserialize;

/// A discovered model from an endpoint.
#[derive(Debug, Clone)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: Option<String>,
    pub context_window: Option<u64>,
}

/// Discover models from a single provider endpoint.
pub async fn discover_provider_models(
    _provider_name: &str,
    config: &ProviderConfig,
) -> Result<Vec<DiscoveredModel>, String> {
    let provider_type = ProviderType::from_str(&config.type_);
    let base_url = config.resolved_base_url();
    let api_key = config.resolved_api_key();

    match provider_type {
        // OpenAI-compatible /v1/models endpoints
        ProviderType::OpenAI
        | ProviderType::OpenAICompatible
        | ProviderType::OpenRouter
        | ProviderType::Groq
        | ProviderType::Mistral
        | ProviderType::Cerebras
        | ProviderType::XAI
        | ProviderType::Azure
        | ProviderType::OpenAIResponses
        | ProviderType::OpenAICodex => {
            // Ollama exposes OpenAI-compat /v1/models but also native /api/tags.
            // Detect by URL and try the native endpoint first for richer metadata.
            if base_url.contains("11434") || base_url.contains("ollama") {
                discover_ollama(&base_url).await
            } else {
                discover_openai_compatible(&base_url, &api_key).await
            }
        }

        ProviderType::Anthropic => Err("Anthropic does not expose a models endpoint".to_string()),

        ProviderType::Google | ProviderType::GoogleVertex | ProviderType::GoogleGeminiCli => {
            discover_gemini(&base_url, &api_key).await
        }

        ProviderType::Bedrock => Err("AWS Bedrock discovery not yet implemented".to_string()),

        ProviderType::GithubCopilot => {
            Err("GitHub Copilot does not expose a models endpoint".to_string())
        }

        ProviderType::Mock => Err("Mock provider".to_string()),
    }
}

/// Non-chat model ID patterns to filter out from discovery results.
const NON_CHAT_PREFIXES: &[&str] = &[
    "dall-e",
    "tts-",
    "whisper-",
    "text-embedding-",
    "omni-moderation-",
    "computer-use-",
    "sora-",
    "gpt-image-",
    "chatgpt-image-",
    "davinci-",
    "babbage-",
    "gpt-audio",    // gpt-audio, gpt-audio-mini, gpt-audio-*
    "gpt-realtime", // gpt-realtime, gpt-realtime-mini, gpt-realtime-*
];

const NON_CHAT_CONTAINS: &[&str] = &[
    "-realtime-preview",
    "-tts",
    "-transcribe",
    "-audio-preview",
    "-instruct-0914", // completions-only instruct variants
];

const NON_CHAT_EXACT: &[&str] = &["gpt-3.5-turbo-instruct"];

/// Returns true if the model ID looks like a chat/completion model.
fn is_chat_model(id: &str) -> bool {
    let lower = id.to_lowercase();

    // Skip fine-tunes
    if lower.starts_with("ft:") {
        return false;
    }

    for exact in NON_CHAT_EXACT {
        if lower == *exact {
            return false;
        }
    }

    for prefix in NON_CHAT_PREFIXES {
        if lower.starts_with(prefix) {
            return false;
        }
    }

    for pattern in NON_CHAT_CONTAINS {
        if lower.contains(pattern) {
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// OpenAI-compatible /v1/models
// ---------------------------------------------------------------------------

async fn discover_openai_compatible(
    base_url: &str,
    api_key: &str,
) -> Result<Vec<DiscoveredModel>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {api_key}"));
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    #[derive(Deserialize)]
    struct ListResponse {
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
    }

    let body: ListResponse = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    Ok(body
        .data
        .into_iter()
        .filter(|m| is_chat_model(&m.id))
        .map(|m| DiscoveredModel {
            id: m.id,
            name: None,
            context_window: None,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Google Gemini
// ---------------------------------------------------------------------------

async fn discover_gemini(base_url: &str, api_key: &str) -> Result<Vec<DiscoveredModel>, String> {
    let base = base_url.trim_end_matches('/');
    let url = if base.ends_with("/v1beta") {
        format!("{base}/models?key={api_key}")
    } else {
        format!("{base}/v1beta/models?key={api_key}")
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    #[derive(Deserialize)]
    #[allow(non_snake_case)]
    struct GeminiResponse {
        models: Vec<GeminiModel>,
    }
    #[derive(Deserialize)]
    #[allow(non_snake_case)]
    struct GeminiModel {
        name: String,
        displayName: Option<String>,
        #[serde(default)]
        inputTokenLimit: Option<u64>,
        #[serde(default)]
        supportedGenerationMethods: Vec<String>,
    }

    let body: GeminiResponse = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    Ok(body
        .models
        .into_iter()
        .filter(|m| {
            m.supportedGenerationMethods
                .iter()
                .any(|method| method.contains("generate") || method.contains("chat"))
        })
        .map(|m| {
            let id = m.name.trim_start_matches("models/").to_string();
            DiscoveredModel {
                id,
                name: m.displayName,
                context_window: m.inputTokenLimit,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Ollama (/api/tags)
// ---------------------------------------------------------------------------

async fn discover_ollama(base_url: &str) -> Result<Vec<DiscoveredModel>, String> {
    // base_url is typically "http://localhost:11434/v1" -- strip /v1 for the
    // native Ollama API which lives at /api/tags.
    let base = base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/');
    let url = format!("{base}/api/tags");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    #[derive(Deserialize)]
    struct TagsResponse {
        models: Vec<OllamaModel>,
    }
    #[derive(Deserialize)]
    struct OllamaModel {
        name: String,
        #[serde(default)]
        details: Option<OllamaDetails>,
    }
    #[derive(Deserialize)]
    struct OllamaDetails {
        #[serde(default)]
        parameter_size: Option<String>,
        #[serde(default)]
        quantization_level: Option<String>,
    }

    let body: TagsResponse = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    Ok(body
        .models
        .into_iter()
        .map(|m| {
            let display = match &m.details {
                Some(d) => {
                    let size = d.parameter_size.as_deref().unwrap_or("");
                    let quant = d.quantization_level.as_deref().unwrap_or("");
                    match (size.is_empty(), quant.is_empty()) {
                        (false, false) => Some(format!("{} ({}, {})", m.name, size, quant)),
                        (false, true) => Some(format!("{} ({})", m.name, size)),
                        _ => None,
                    }
                }
                None => None,
            };
            DiscoveredModel {
                id: m.name,
                name: display,
                context_window: None,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_non_chat_models() {
        // Chat models pass
        assert!(is_chat_model("gpt-4o"));
        assert!(is_chat_model("gpt-4o-mini"));
        assert!(is_chat_model("claude-3-opus"));
        assert!(is_chat_model("o3"));
        assert!(is_chat_model("o4-mini"));
        assert!(is_chat_model("gpt-5.2-codex"));
        assert!(is_chat_model("gpt-5"));
        assert!(is_chat_model("gpt-4.1"));

        // Non-chat models filtered
        assert!(!is_chat_model("dall-e-3"));
        assert!(!is_chat_model("tts-1-hd"));
        assert!(!is_chat_model("whisper-1"));
        assert!(!is_chat_model("text-embedding-3-large"));
        assert!(!is_chat_model("omni-moderation-latest"));
        assert!(!is_chat_model("gpt-4o-realtime-preview"));
        assert!(!is_chat_model("gpt-4o-mini-tts-2025-03-20"));
        assert!(!is_chat_model("ft:gpt-3.5-turbo-1106:org::abc123"));
        assert!(!is_chat_model("sora-2-pro"));
        assert!(!is_chat_model("gpt-image-1"));
        assert!(!is_chat_model("gpt-4o-audio-preview"));
        assert!(!is_chat_model("gpt-audio-mini-2025-10-06"));
        assert!(!is_chat_model("gpt-audio"));
        assert!(!is_chat_model("gpt-audio-2025-08-28"));
        assert!(!is_chat_model("gpt-realtime"));
        assert!(!is_chat_model("gpt-realtime-mini"));
        assert!(!is_chat_model("gpt-3.5-turbo-instruct"));
        assert!(!is_chat_model("gpt-3.5-turbo-instruct-0914"));
        assert!(!is_chat_model("chatgpt-image-latest"));
        assert!(!is_chat_model("computer-use-preview"));
    }
}
