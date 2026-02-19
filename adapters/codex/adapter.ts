/**
 * Codex CLI adapter for eavs model export
 *
 * Generates Codex-compatible config.yaml from eavs provider configuration.
 * Codex stores config in ~/.codex/config.yaml
 *
 * All providers route through eavs as the proxy. Each provider gets a
 * unique eavs-prefixed base URL so Codex sends requests through eavs.
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

/** Map eavs provider type to Codex provider display name */
function codexDisplayName(provider: EavsProvider): string {
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

  const name = provider.name;
  const type = provider.type;
  return displayNames[name] ?? displayNames[type] ?? name;
}

/** Build eavs provider entries in Codex format.
 *  All providers point at eavs with the provider-prefixed URL. */
function buildEavsProviders(
  providers: EavsProvider[],
  baseUrl: string,
  apiKey: string
): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  const base = baseUrl.replace(/\/+$/, "");

  for (const provider of providers) {
    if (provider.name === "default") continue;

    const key = `${MANAGED_PREFIX}${provider.name}`;

    result[key] = {
      name: codexDisplayName(provider),
      baseURL: `${base}/${provider.name}/v1`,
      envKey: "EAVS_API_KEY",
    };
  }

  return result;
}

/** Pick a sensible default model from the providers */
function pickDefaultModel(providers: EavsProvider[]): string {
  // Prefer codex models, then openai, then first available
  for (const provider of providers) {
    if (provider.name === "default") continue;
    for (const model of provider.models) {
      if (model.id.includes("codex")) return model.id;
    }
  }
  const first = providers.find((p) => p.name !== "default");
  if (first && first.models.length > 0) return first.models[0].id;
  return "codex-mini-latest";
}

/** Build complete Codex config */
function buildCodexConfig(
  providers: EavsProvider[],
  baseUrl: string,
  apiKey: string
): Record<string, unknown> {
  const eavsProviders = buildEavsProviders(providers, baseUrl, apiKey);

  // Use the first eavs provider as active
  const activeProvider = Object.keys(eavsProviders)[0] ?? "openai";

  return {
    model: pickDefaultModel(providers),
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
    let existing: Record<string, unknown>;
    try {
      existing = parseYaml(req.existing);
    } catch {
      return buildCodexConfig(req.providers, req.base_url, req.api_key);
    }

    const providers = (existing.providers as Record<string, unknown>) ?? {};

    // Remove all eavs-managed entries
    for (const key of Object.keys(providers)) {
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
  const stack: { obj: Record<string, unknown>; indent: number }[] = [];

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;

    const indent = line.length - line.trimStart().length;
    const match = trimmed.match(/^([^:]+):\s*(.*)$/);

    if (match) {
      const [, key, value] = match;

      while (stack.length > 0 && indent <= stack[stack.length - 1].indent) {
        stack.pop();
      }

      const target = stack.length > 0 ? stack[stack.length - 1].obj : result;

      if (value) {
        target[key] = parseValue(value);
      } else {
        const newObj: Record<string, unknown> = {};
        target[key] = newObj;
        stack.push({ obj: newObj, indent });
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
    return value
      .slice(1, -1)
      .split(",")
      .map((s) => s.trim());
  }
  if (value.startsWith('"') && value.endsWith('"')) {
    return value.slice(1, -1);
  }
  if (value.startsWith("'") && value.endsWith("'")) {
    return value.slice(1, -1);
  }
  return value;
}
