/**
 * Goose adapter for eavs model export
 *
 * Generates Goose-compatible config.yaml from eavs provider configuration.
 * Goose stores config in ~/.config/goose/config.yaml
 *
 * Goose uses environment variables for provider configuration rather than
 * a structured providers object. We generate the relevant env vars.
 */

import type {
  AdapterInfo,
  ExportRequest,
  MergeRequest,
  EavsProvider,
} from "../types/index.ts";
import { runAdapter } from "../types/index.ts";

const MANAGED_PREFIX = "EAVS_";

/** Map eavs provider type to Goose env vars */
function gooseEnvVars(provider: EavsProvider, apiKey: string): Record<string, string> {
  const vars: Record<string, string> = {};
  const name = provider.name.toUpperCase().replace(/-/g, "_");
  const type = provider.type.toUpperCase().replace(/-/g, "_");

  // Goose uses specific env vars per provider type
  switch (provider.type) {
    case "openai":
      vars.OPENAI_BASE_PATH = `${providerBaseUrl(provider)}/chat/completions`;
      vars.OPENAI_API_KEY = apiKey;
      break;
    case "anthropic":
      vars.ANTHROPIC_BASE_PATH = `${providerBaseUrl(provider)}/messages`;
      vars.ANTHROPIC_API_KEY = apiKey;
      break;
    case "azure":
      vars.AZURE_OPENAI_ENDPOINT = providerBaseUrl(provider);
      vars.AZURE_OPENAI_API_KEY = apiKey;
      break;
    case "google":
    case "google-vertex":
      vars.GOOGLE_API_KEY = apiKey;
      break;
    case "ollama":
      vars.OLLAMA_HOST = "localhost:11434";
      break;
    default:
      // Generic provider config using provider name
      vars[`${name}_BASE_URL`] = providerBaseUrl(provider);
      vars[`${name}_API_KEY`] = apiKey;
  }

  return vars;
}

/** Get base URL for provider */
function providerBaseUrl(provider: EavsProvider): string {
  const defaults: Record<string, string> = {
    openai: "https://api.openai.com/v1",
    anthropic: "https://api.anthropic.com/v1",
    google: "https://generativelanguage.googleapis.com/v1beta",
    "google-vertex": "https://generativelanguage.googleapis.com/v1beta",
    azure: "https://YOUR_PROJECT_NAME.openai.azure.com/",
    mistral: "https://api.mistral.ai/v1",
    groq: "https://api.groq.com/openai/v1",
    cerebras: "https://api.cerebras.ai/v1",
    xai: "https://api.x.ai/v1",
    openrouter: "https://openrouter.ai/api/v1",
    ollama: "http://localhost:11434",
    deepseek: "https://api.deepseek.com",
  };

  return defaults[provider.type] ?? "http://localhost:3000";
}

/** Build Goose config with provider env vars */
function buildGooseConfig(
  providers: EavsProvider[],
  _baseUrl: string,
  apiKey: string
): Record<string, unknown> {
  const envVars: Record<string, string> = {};

  // Collect env vars from all providers
  for (const provider of providers) {
    if (provider.name === "default") continue;
    const vars = gooseEnvVars(provider, apiKey);
    Object.assign(envVars, vars);
  }

  return {
    // Include env vars at top level (Goose reads these)
    ...envVars,
    // Default extensions (empty)
    extensions: {},
  };
}

runAdapter({
  info(): AdapterInfo {
    return {
      name: "goose",
      displayName: "Goose",
      version: "1.0.0",
      outputFile: "config.yaml",
      defaultPath: "~/.config/goose/config.yaml",
      description: "Generates ~/.config/goose/config.yaml with provider env vars",
      managedPrefix: MANAGED_PREFIX,
    };
  },

  export(req: ExportRequest): Record<string, unknown> {
    return buildGooseConfig(req.providers, req.base_url, req.api_key);
  },

  merge(req: MergeRequest): Record<string, unknown> {
    // Parse existing config
    let existing: Record<string, unknown>;
    try {
      existing = parseYaml(req.existing);
    } catch {
      return buildGooseConfig(req.providers, req.base_url, req.api_key);
    }

    // Remove eavs-managed env vars (prefixed with EAVS_ or known provider vars)
    const managedVars = [
      "OPENAI_BASE_PATH",
      "OPENAI_API_KEY",
      "ANTHROPIC_BASE_PATH",
      "ANTHROPIC_API_KEY",
      "AZURE_OPENAI_ENDPOINT",
      "AZURE_OPENAI_API_KEY",
      "GOOGLE_API_KEY",
      "OLLAMA_HOST",
    ];

    for (const key of Object.keys(existing)) {
      if (key.startsWith(MANAGED_PREFIX) || managedVars.includes(key)) {
        delete existing[key];
      }
    }

    // Add new eavs env vars
    const eavsVars: Record<string, string> = {};
    for (const provider of req.providers) {
      if (provider.name === "default") continue;
      const vars = gooseEnvVars(provider, req.api_key);
      Object.assign(eavsVars, vars);
    }

    // Merge and return
    return {
      ...eavsVars,
      ...existing,
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
  if (value === "null" || value === "~") return null;
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
