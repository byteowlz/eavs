/**
 * Aider adapter for eavs model export
 *
 * Generates Aider-compatible configuration.
 * Aider can use .aider.conf.yml in project root or ~/.aider.conf.yml
 *
 * All requests route through eavs as the proxy. Aider's config is simpler
 * than most -- it primarily needs a model name, API key, and base URL.
 * When routing through eavs, we use the eavs provider-prefixed URL.
 */

import type {
  AdapterInfo,
  ExportRequest,
  MergeRequest,
  EavsProvider,
} from "../types/index.ts";
import { runAdapter } from "../types/index.ts";

/** Pick the best default model from available providers */
function pickDefaultModel(providers: EavsProvider[]): string {
  // Prefer anthropic sonnet, then openai gpt-4o, then first available
  for (const provider of providers) {
    if (provider.name === "default") continue;
    for (const model of provider.models) {
      if (model.id.includes("sonnet")) return model.id;
    }
  }
  for (const provider of providers) {
    if (provider.name === "default") continue;
    for (const model of provider.models) {
      if (model.id.includes("gpt-4o")) return model.id;
    }
  }
  const first = providers.find((p) => p.name !== "default");
  if (first && first.models.length > 0) return first.models[0].id;
  return "gpt-4o";
}

/** Find which provider owns the default model */
function findProviderForModel(
  providers: EavsProvider[],
  modelId: string
): EavsProvider | undefined {
  for (const provider of providers) {
    if (provider.name === "default") continue;
    for (const model of provider.models) {
      if (model.id === modelId) return provider;
    }
  }
  return providers.find((p) => p.name !== "default");
}

/** Build Aider config routing through eavs */
function buildAiderConfig(
  providers: EavsProvider[],
  baseUrl: string,
  apiKey: string
): Record<string, unknown> {
  const base = baseUrl.replace(/\/+$/, "");
  const model = pickDefaultModel(providers);
  const provider = findProviderForModel(providers, model);

  const config: Record<string, unknown> = {
    model: model,
    // Aider uses openai_api_key for the API key and openai_api_base for the
    // base URL, which works for OpenAI-compatible endpoints (which eavs is).
    openai_api_key: apiKey,
  };

  if (provider) {
    config.openai_api_base = `${base}/${provider.name}/v1`;
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

    // Preserve non-eavs settings (anything we don't manage)
    const preserved: Record<string, unknown> = {};
    const managedKeys = ["model", "openai_api_key", "openai_api_base", "api_key", "api_key_env_var", "base_url"];
    for (const [key, value] of Object.entries(existing)) {
      if (!managedKeys.includes(key)) {
        preserved[key] = value;
      }
    }

    return {
      ...preserved,
      ...newConfig,
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
