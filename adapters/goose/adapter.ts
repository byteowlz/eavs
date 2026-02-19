/**
 * Goose adapter for eavs model export
 *
 * Generates Goose-compatible config.yaml from eavs provider configuration.
 * Goose stores config in ~/.config/goose/config.yaml
 *
 * All requests route through eavs as the proxy. Goose uses environment
 * variables for provider configuration, so we set the base URLs and API
 * keys to point at eavs endpoints.
 */

import type {
  AdapterInfo,
  ExportRequest,
  MergeRequest,
  EavsProvider,
} from "../types/index.ts";
import { runAdapter } from "../types/index.ts";

const MANAGED_PREFIX = "EAVS_";

/** Map eavs provider type to Goose env vars, routing through eavs */
function gooseEnvVars(
  provider: EavsProvider,
  baseUrl: string,
  apiKey: string
): Record<string, string> {
  const vars: Record<string, string> = {};
  const base = baseUrl.replace(/\/+$/, "");
  const providerUrl = `${base}/${provider.name}/v1`;
  const name = provider.name.toUpperCase().replace(/-/g, "_");

  // Goose uses specific env vars per provider type
  switch (provider.type) {
    case "openai":
      vars.OPENAI_BASE_PATH = `${providerUrl}/chat/completions`;
      vars.OPENAI_API_KEY = apiKey;
      break;
    case "anthropic":
      vars.ANTHROPIC_BASE_PATH = `${providerUrl}/messages`;
      vars.ANTHROPIC_API_KEY = apiKey;
      break;
    case "azure":
      vars.AZURE_OPENAI_ENDPOINT = providerUrl;
      vars.AZURE_OPENAI_API_KEY = apiKey;
      break;
    case "google":
    case "google-vertex":
      vars.GOOGLE_API_KEY = apiKey;
      break;
    case "ollama":
      // Ollama through eavs still uses the eavs URL
      vars.OLLAMA_HOST = providerUrl;
      break;
    default:
      // Generic provider config using provider name
      vars[`${name}_BASE_URL`] = providerUrl;
      vars[`${name}_API_KEY`] = apiKey;
  }

  return vars;
}

/** Build Goose config with provider env vars routing through eavs */
function buildGooseConfig(
  providers: EavsProvider[],
  baseUrl: string,
  apiKey: string
): Record<string, unknown> {
  const envVars: Record<string, string> = {};

  for (const provider of providers) {
    if (provider.name === "default") continue;
    const vars = gooseEnvVars(provider, baseUrl, apiKey);
    Object.assign(envVars, vars);
  }

  return {
    ...envVars,
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
    let existing: Record<string, unknown>;
    try {
      existing = parseYaml(req.existing);
    } catch {
      return buildGooseConfig(req.providers, req.base_url, req.api_key);
    }

    // Remove eavs-managed env vars
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
      const vars = gooseEnvVars(provider, req.base_url, req.api_key);
      Object.assign(eavsVars, vars);
    }

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
