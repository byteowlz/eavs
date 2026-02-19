/**
 * Codex CLI adapter for eavs model export
 *
 * Generates Codex-compatible config.yaml from eavs provider configuration.
 * Codex stores config in ~/.codex/config.yaml
 *
 * Supports:
 *   - export: generate a complete config.yaml from scratch
 *   - merge: update eavs-managed providers in an existing config.yaml
 */

import type {
  AdapterInfo,
  ExportRequest,
  MergeRequest,
  EavsProvider,
} from "../types/index.ts";
import { runAdapter } from "../types/index.ts";

const MANAGED_PREFIX = "eavs-";

/** Map eavs provider type to Codex provider name */
function codexProviderName(provider: EavsProvider): string | null {
  // If provider name already matches a codex provider, use it
  const name = provider.name.toLowerCase();

  const knownProviders: Record<string, string> = {
    openai: "openai",
    anthropic: "anthropic",
    google: "gemini",
    "google-vertex": "gemini",
    azure: "azure",
    mistral: "mistral",
    groq: "groq",
    cerebras: "cerebras",
    xai: "xai",
    openrouter: "openrouter",
    ollama: "ollama",
    deepseek: "deepseek",
    bedrock: "bedrock",
  };

  return knownProviders[name] ?? knownProviders[provider.type] ?? null;
}

/** Get display name for provider */
function codexDisplayName(provider: EavsProvider): string {
  const name = provider.name;
  const type = provider.type;

  const displayNames: Record<string, string> = {
    openai: "OpenAI",
    anthropic: "Anthropic",
    google: "Google Gemini",
    "google-vertex": "Google Vertex",
    azure: "Azure OpenAI",
    mistral: "Mistral",
    groq: "Groq",
    cerebras: "Cerebras",
    xai: "xAI",
    openrouter: "OpenRouter",
    ollama: "Ollama",
    deepseek: "DeepSeek",
    bedrock: "AWS Bedrock",
  };

  return displayNames[name] ?? displayNames[type] ?? name;
}

/** Build eavs provider entries in Codex format */
function buildEavsProviders(
  providers: EavsProvider[],
  _baseUrl: string,
  apiKey: string
): Record<string, unknown> {
  const result: Record<string, unknown> = {};

  for (const provider of providers) {
    if (provider.name === "default") continue;

    const codexName = codexProviderName(provider);
    if (!codexName) continue;

    // Determine env key name based on provider
    const envKey = providerEnvKey(provider);

    result[codexName] = {
      name: codexDisplayName(provider),
      baseURL: providerBaseUrl(provider),
      envKey: envKey,
    };
  }

  return result;
}

/** Get environment variable key for provider */
function providerEnvKey(provider: EavsProvider): string {
  const name = provider.name.toUpperCase().replace(/-/g, "_");
  const type = provider.type.toUpperCase().replace(/-/g, "_");

  const envKeys: Record<string, string> = {
    OPENAI: "OPENAI_API_KEY",
    ANTHROPIC: "ANTHROPIC_API_KEY",
    GOOGLE: "GEMINI_API_KEY",
    GEMINI: "GEMINI_API_KEY",
    AZURE: "AZURE_OPENAI_API_KEY",
    MISTRAL: "MISTRAL_API_KEY",
    GROQ: "GROQ_API_KEY",
    CEREBRAS: "CEREBRAS_API_KEY",
    XAI: "XAI_API_KEY",
    OPENROUTER: "OPENROUTER_API_KEY",
    OLLAMA: "OLLAMA_API_KEY",
    DEEPSEEK: "DEEPSEEK_API_KEY",
    BEDROCK: "AWS_ACCESS_KEY_ID",
  };

  return envKeys[name] ?? envKeys[type] ?? `${name}_API_KEY`;
}

/** Get base URL for provider */
function providerBaseUrl(provider: EavsProvider): string {
  // If provider has base_url in config, use it
  // Otherwise use default based on type
  const defaults: Record<string, string> = {
    openai: "https://api.openai.com/v1",
    anthropic: "https://api.anthropic.com/v1",
    google: "https://generativelanguage.googleapis.com/v1beta",
    "google-vertex": "https://generativelanguage.googleapis.com/v1beta",
    azure: "https://YOUR_PROJECT_NAME.openai.azure.com/openai",
    mistral: "https://api.mistral.ai/v1",
    groq: "https://api.groq.com/openai/v1",
    cerebras: "https://api.cerebras.ai/v1",
    xai: "https://api.x.ai/v1",
    openrouter: "https://openrouter.ai/api/v1",
    ollama: "http://localhost:11434/v1",
    deepseek: "https://api.deepseek.com",
  };

  return defaults[provider.type] ?? "http://localhost:3000/v1";
}

/** Build complete Codex config */
function buildCodexConfig(
  providers: EavsProvider[],
  baseUrl: string,
  apiKey: string
): Record<string, unknown> {
  const eavsProviders = buildEavsProviders(providers, baseUrl, apiKey);

  // If there's a default provider, use it
  const defaultProvider = providers.find((p) => p.name === "default");
  const activeProvider = defaultProvider
    ? codexProviderName(defaultProvider)
    : Object.keys(eavsProviders)[0] ?? "openai";

  return {
    model: "codex-mini-latest",
    provider: activeProvider,
    providers: eavsProviders,
    disableResponseStorage: false,
    flexMode: false,
    reasoningEffort: "high",
    history: {
      maxSize: 1000,
      saveHistory: true,
      sensitivePatterns: [],
    },
    tools: {
      shell: {
        maxBytes: 10240,
        maxLines: 256,
      },
    },
  };
}

runAdapter({
  info(): AdapterInfo {
    return {
      name: "codex",
      displayName: "Codex CLI",
      version: "1.0.0",
      outputFile: "config.yaml",
      defaultPath: "~/.codex/config.yaml",
      description: "Generates ~/.codex/config.yaml for the Codex CLI",
      managedPrefix: MANAGED_PREFIX,
    };
  },

  export(req: ExportRequest): Record<string, unknown> {
    return buildCodexConfig(req.providers, req.base_url, req.api_key);
  },

  merge(req: MergeRequest): Record<string, unknown> {
    // Parse existing YAML config
    let existing: Record<string, unknown>;
    try {
      existing = parseYaml(req.existing);
    } catch {
      // If existing is invalid, treat as fresh export
      return buildCodexConfig(req.providers, req.base_url, req.api_key);
    }

    const providers = (existing.providers as Record<string, unknown>) ?? {};

    // Remove all eavs-managed entries
    for (const key of Object.keys(providers)) {
      // Remove providers that look like eavs-managed ones
      // (We can't easily identify them, so we document that users
      // should use the "eavs-" prefix if they want merge to work perfectly)
      if (key.startsWith(MANAGED_PREFIX)) {
        delete providers[key];
      }
    }

    // Add the new eavs entries
    const eavsProviders = buildEavsProviders(
      req.providers,
      req.base_url,
      req.api_key
    );
    Object.assign(providers, eavsProviders);

    // Update the provider field if the current one was removed
    const currentProvider = existing.provider as string;
    if (currentProvider && !providers[currentProvider]) {
      existing.provider = Object.keys(eavsProviders)[0] ?? "openai";
    }

    existing.providers = providers;
    return existing;
  },
});

/** Simple YAML parser for basic Codex configs */
function parseYaml(yaml: string): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  const lines = yaml.split("\n");
  let currentKey = "";
  let currentObj: Record<string, unknown> = result;
  const stack: { key: string; obj: Record<string, unknown> }[] = [];

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;

    const indent = line.length - line.trimStart().length;
    const match = trimmed.match(/^([^:]+):\s*(.*)$/);

    if (match) {
      const [, key, value] = match;

      // Pop stack to correct level based on indentation
      while (stack.length > 0 && indent <= stack[stack.length - 1].key.length) {
        stack.pop();
      }

      const target = stack.length > 0 ? stack[stack.length - 1].obj : result;

      if (value) {
        // Scalar value
        target[key] = parseValue(value);
      } else {
        // Object or array - prepare for nested content
        const newObj: Record<string, unknown> = {};
        target[key] = newObj;
        stack.push({ key, obj: newObj });
      }
    }
  }

  return result;
}

function parseValue(value: string): unknown {
  value = value.trim();
  if (value === "true") return true;
  if (value === "false") return false;
  if (value === "null" || value === "~") return null;
  if (/^\d+$/.test(value)) return parseInt(value, 10);
  if (/^\d+\.\d+$/.test(value)) return parseFloat(value);
  if (/^\[.*\]$/.test(value)) {
    // Simple array parsing
    return value
      .slice(1, -1)
      .split(",")
      .map((s) => s.trim());
  }
  // String (remove surrounding quotes if present)
  if (value.startsWith('"') && value.endsWith('"')) {
    return value.slice(1, -1);
  }
  if (value.startsWith("'") && value.endsWith("'")) {
    return value.slice(1, -1);
  }
  return value;
}
