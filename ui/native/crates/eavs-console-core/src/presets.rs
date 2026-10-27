//! Provider presets: the known way to connect each upstream, so adding a
//! provider is "pick it, paste a key" (or "sign in"), not a config exercise.

/// How a preset authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// Paste an API key; `key_url` is where to create one.
    ApiKey { key_url: &'static str },
    /// A subscription login held by the gateway (`eavs login <login>`).
    Subscription { login: &'static str },
    /// Local runtime without a key.
    None,
}

/// A connectable upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    /// Short description shown under the label.
    pub blurb: &'static str,
    /// eavs provider type.
    pub provider_type: &'static str,
    pub base_url: &'static str,
    pub auth: Auth,
    /// The gateway reads plan/credit usage for this preset.
    pub usage: bool,
    /// Watch availability (local runtimes).
    pub probe: bool,
    /// Suggested provider name.
    pub name: &'static str,
}

/// Presets in display order: local first, then subscriptions, then API keys.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "local",
        label: "Local runtime",
        blurb: "llama.cpp, vLLM, ollama, LM Studio, llama-swap",
        provider_type: "openai-compatible",
        base_url: "http://127.0.0.1:8080/v1",
        auth: Auth::None,
        usage: false,
        probe: true,
        name: "local",
    },
    Preset {
        id: "claude",
        label: "Claude subscription",
        blurb: "Pro / Max plan, with session, weekly and extra-usage meters",
        provider_type: "anthropic",
        base_url: "https://api.anthropic.com",
        auth: Auth::Subscription { login: "anthropic" },
        usage: true,
        probe: false,
        name: "claude",
    },
    Preset {
        id: "codex",
        label: "ChatGPT / Codex subscription",
        blurb: "Plus / Pro plan through the Codex backend",
        provider_type: "openai-codex",
        base_url: "https://chatgpt.com/backend-api",
        auth: Auth::Subscription {
            login: "openai-codex",
        },
        usage: true,
        probe: false,
        name: "codex",
    },
    Preset {
        id: "opencode-go",
        label: "OpenCode Go",
        blurb: "Open models with session, weekly and monthly caps",
        provider_type: "opencode-go",
        base_url: "https://opencode.ai/zen/go/v1",
        auth: Auth::ApiKey {
            key_url: "https://opencode.ai/auth",
        },
        usage: true,
        probe: false,
        name: "opencode-go",
    },
    Preset {
        id: "zai",
        label: "Z.ai GLM Coding Plan",
        blurb: "GLM models on the coding-plan endpoint",
        provider_type: "openai-compatible",
        base_url: "https://api.z.ai/api/coding/paas/v4",
        auth: Auth::ApiKey {
            key_url: "https://z.ai/manage-apikey/apikey-list",
        },
        usage: true,
        probe: false,
        name: "zai",
    },
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        blurb: "Hundreds of models, prepaid credits",
        provider_type: "openrouter",
        base_url: "https://openrouter.ai/api/v1",
        auth: Auth::ApiKey {
            key_url: "https://openrouter.ai/keys",
        },
        usage: true,
        probe: false,
        name: "openrouter",
    },
    Preset {
        id: "anthropic-api",
        label: "Anthropic API",
        blurb: "Pay-as-you-go API key",
        provider_type: "anthropic",
        base_url: "https://api.anthropic.com",
        auth: Auth::ApiKey {
            key_url: "https://console.anthropic.com/settings/keys",
        },
        usage: false,
        probe: false,
        name: "anthropic-api",
    },
    Preset {
        id: "openai",
        label: "OpenAI API",
        blurb: "Pay-as-you-go API key",
        provider_type: "openai",
        base_url: "https://api.openai.com/v1",
        auth: Auth::ApiKey {
            key_url: "https://platform.openai.com/api-keys",
        },
        usage: false,
        probe: false,
        name: "openai",
    },
    Preset {
        id: "xai",
        label: "xAI",
        blurb: "Grok models, API key",
        provider_type: "xai",
        base_url: "https://api.x.ai/v1",
        auth: Auth::ApiKey {
            key_url: "https://console.x.ai",
        },
        usage: false,
        probe: false,
        name: "xai",
    },
    Preset {
        id: "gemini",
        label: "Google Gemini",
        blurb: "AI Studio API key",
        provider_type: "google",
        base_url: "https://generativelanguage.googleapis.com",
        auth: Auth::ApiKey {
            key_url: "https://aistudio.google.com/apikey",
        },
        usage: false,
        probe: false,
        name: "gemini",
    },
    Preset {
        id: "mistral",
        label: "Mistral",
        blurb: "API key",
        provider_type: "mistral",
        base_url: "https://api.mistral.ai/v1",
        auth: Auth::ApiKey {
            key_url: "https://console.mistral.ai/api-keys",
        },
        usage: false,
        probe: false,
        name: "mistral",
    },
    Preset {
        id: "groq",
        label: "Groq",
        blurb: "API key",
        provider_type: "groq",
        base_url: "https://api.groq.com/openai/v1",
        auth: Auth::ApiKey {
            key_url: "https://console.groq.com/keys",
        },
        usage: false,
        probe: false,
        name: "groq",
    },
    Preset {
        id: "custom",
        label: "Other OpenAI-compatible",
        blurb: "Any endpoint that speaks /v1/chat/completions",
        provider_type: "openai-compatible",
        base_url: "",
        auth: Auth::ApiKey { key_url: "" },
        usage: false,
        probe: false,
        name: "",
    },
];

/// Look a preset up by id.
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// A name for a new provider that does not collide with `taken`.
pub fn unique_name(base: &str, taken: &[String]) -> String {
    if base.is_empty() {
        return String::new();
    }
    if !taken.iter().any(|t| t == base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken.iter().any(|t| t == candidate))
        .unwrap_or_else(|| base.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_ids_are_unique_and_names_valid() {
        let mut ids: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PRESETS.len());
        for preset in PRESETS {
            assert!(
                preset
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{}",
                preset.id
            );
        }
    }

    #[test]
    fn unique_names_avoid_collisions() {
        let taken = vec!["local".to_owned(), "local-2".to_owned()];
        assert_eq!(unique_name("local", &taken), "local-3");
        assert_eq!(unique_name("rtx", &taken), "rtx");
    }
}
