/**
 * Aider adapter for eavs model export
 *
 * Generates Aider-compatible configuration.
 * Aider can use .aider.conf.yml in project root or ~/.aider.conf.yml
 *
 * Aider's config is simpler - mostly model selection and API keys.
 * It doesn't have a structured providers object like Pi.
 */

import type {
  AdapterInfo,
  ExportRequest,
  MergeRequest,
  EavsProvider,
} from "../types/index.ts";
import { runAdapter } from "../types/index.ts";

/** Get default model for provider */
function defaultModelForProvider(provider: EavsProvider): string {
  const defaults: Record<string, string> = {
    openai: "gpt-4o",
    anthropic: "claude-sonnet-4",
    google: "gemini-2.0-flash",
    "google-vertex": "gemini-2.0-flash",
    azure: "gpt-4o",
    mistral: "mistral-large",
    groq: "llama-3.3-70b",
    xai: "grok-2",
    openrouter: "openai/gpt-4o",
  };

  if (provider.models.length > 0) {
    return provider.models[0].id;
  }

  return defaults[provider.type] ?? "gpt-4o";
}

/** Get API key env var for provider */
function apiKeyEnvVar(providerType: string): string {
  const envVars: Record<string, string> = {
    openai: "OPENAI_API_KEY",
    anthropic: "ANTHROPIC_API_KEY",
    google: "GEMINI_API_KEY",
    "google-vertex": "GEMINI_API_KEY",
    azure: "AZURE_OPENAI_API_KEY",
    mistral: "MISTRAL_API_KEY",
    groq: "GROQ_API_KEY",
    cerebras: "CEREBRAS_API_KEY",
    xai: "XAI_API_KEY",
    openrouter: "OPENROUTER_API_KEY",
    bedrock: "AWS_ACCESS_KEY_ID",
  };

  return envVars[providerType] ?? "OPENAI_API_KEY";
}

/** Build Aider config */
function buildAiderConfig(
  providers: EavsProvider[],
  _baseUrl: string,
  _apiKey: string
): Record<string, unknown> {
  // Find the default or first provider
  const defaultProvider =
    providers.find((p) => p.name === "default") ?? providers[0];

  if (!defaultProvider) {
    return {
      model: "gpt-4o",
    };
  }

  const model = defaultModelForProvider(defaultProvider);

  // Aider uses a simple model identifier
  // Format: provider/model-name or just model-name
  let aiderModel = model;
  if (!model.includes("/")) {
    // Check if we need to prefix with provider name
    const needsPrefix = ["ollama", "openrouter", "bedrock"].includes(
      defaultProvider.type
    );
    if (needsPrefix) {
      aiderModel = `${defaultProvider.type}/${model}`;
    }
  }

  const config: Record<string, unknown> = {
    model: aiderModel,
  };

  // Add API key env var comment (not actual key for security)
  const envVar = apiKeyEnvVar(defaultProvider.type);
  config.api_key_env_var = envVar;

  // Add base URL if non-standard
  if (defaultProvider.type === "ollama") {
    config.base_url = "http://localhost:11434/v1";
  }

  return config;
}

runAdapter({
  info(): AdapterInfo {
    return {
      name: "aider",
      displayName: "Aider",
      version: "1.0.0",
      outputFile: ".aider.conf.yml",
      defaultPath: "~/.aider.conf.yml",
      description:
        "Generates .aider.conf.yml for Aider (AI coding assistant)",
      managedPrefix: "# eavs-managed",
    };
  },

  export(req: ExportRequest): Record<string, unknown> {
    return buildAiderConfig(req.providers, req.base_url, req.api_key);
  },

  merge(req: MergeRequest): Record<string, unknown> {
    // Parse existing YAML
    let existing: Record<string, unknown>;
    try {
      existing = parseYaml(req.existing);
    } catch {
      return buildAiderConfig(req.providers, req.base_url, req.api_key);
    }

    // Build new config
    const newConfig = buildAiderConfig(
      req.providers,
      req.base_url,
      req.api_key
    );

    // Preserve non-eavs settings
    const preserved: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(existing)) {
      if (key !== "model" && key !== "api_key" && key !== "base_url") {
        preserved[key] = value;
      }
    }

    return {
      ...preserved,
      ...newConfig,
      // Mark as eavs-managed
      _comment: "eavs-managed configuration - model settings from eavs",
    };
  },
});

/** Simple YAML parser */
function parseYaml(yaml: string): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  const lines = yaml.split("\n");

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;

    const match = trimmed.match(/^([^:]+):\s*(.*)$/);
    if (match) {
      const [, key, value] = match;
      result[key] = parseValue(value);
    }
  }

  return result;
}

function parseValue(value: string): unknown {
  value = value.trim();
  if (value === "true") return true;
  if (value === "false") return false;
  if (value === "null") return null;
  if (/^\d+$/.test(value)) return parseInt(value, 10);
  if (/^\d+\.\d+$/.test(value)) return parseFloat(value);
  if (value.startsWith('"') && value.endsWith('"')) {
    return value.slice(1, -1);
  }
  if (value.startsWith("'") && value.endsWith("'")) {
    return value.slice(1, -1);
  }
  return value;
}
