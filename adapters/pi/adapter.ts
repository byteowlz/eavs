/**
 * Pi adapter for eavs model export
 *
 * Generates Pi-compatible models.json from eavs provider configuration.
 * Pi expects providers keyed as "eavs-{name}" with camelCase model fields.
 *
 * Supports two modes:
 *   - export: generate a complete models.json from scratch
 *   - merge:  update eavs-managed providers in an existing models.json,
 *             preserving any non-eavs providers the user configured directly
 */

import type {
  AdapterInfo,
  ExportRequest,
  MergeRequest,
  EavsProvider,
} from "../types/index.ts";
import { runAdapter } from "../types/index.ts";

const MANAGED_PREFIX = "eavs-";

/** Map eavs provider type to Pi API type */
function piApiType(provider: EavsProvider): string | null {
  if (provider.pi_api) return provider.pi_api;

  const mapping: Record<string, string> = {
    openai: "openai-responses",
    "openai-responses": "openai-responses",
    "openai-codex": "openai-codex-responses",
    anthropic: "anthropic-messages",
    google: "google-generative-ai",
    "google-vertex": "google-generative-ai",
    azure: "openai-responses",
    mistral: "openai-completions",
    groq: "openai-completions",
    cerebras: "openai-completions",
    xai: "openai-completions",
    openrouter: "openai-completions",
    "openai-compatible": "openai-completions",
    "github-copilot": "openai-responses",
    bedrock: "anthropic-messages",
  };

  return mapping[provider.type] ?? null;
}

/** Build eavs provider entries in Pi format */
function buildEavsProviders(
  providers: EavsProvider[],
  baseUrl: string,
  apiKey: string
): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  const base = baseUrl.replace(/\/+$/, "");

  for (const provider of providers) {
    if (provider.name === "default") continue;

    const api = piApiType(provider);
    if (!api) continue;

    const models = provider.models.map((m) => ({
      id: m.id,
      name: m.name || m.id,
      reasoning: m.reasoning,
      input: m.input?.length ? m.input : ["text"],
      contextWindow: m.context_window,
      maxTokens: m.max_tokens,
      cost: {
        input: m.cost?.input ?? 0,
        output: m.cost?.output ?? 0,
        cacheRead: m.cost?.cache_read ?? 0,
        cacheWrite: 0,
      },
    }));

    result[`${MANAGED_PREFIX}${provider.name}`] = {
      baseUrl: `${base}/${provider.name}/v1`,
      api,
      apiKey: apiKey,
      models,
    };
  }

  return result;
}

runAdapter({
  info(): AdapterInfo {
    return {
      name: "pi",
      displayName: "Pi Coding Agent",
      version: "1.0.0",
      outputFile: "models.json",
      defaultPath: "~/.pi/agent/models.json",
      description: "Generates ~/.pi/agent/models.json for the Pi coding agent",
      managedPrefix: MANAGED_PREFIX,
    };
  },

  export(req: ExportRequest): Record<string, unknown> {
    return {
      providers: buildEavsProviders(req.providers, req.base_url, req.api_key),
    };
  },

  merge(req: MergeRequest): Record<string, unknown> {
    // Parse the existing config
    let existing: { providers?: Record<string, unknown> };
    try {
      existing = JSON.parse(req.existing);
    } catch {
      // If existing file is invalid, treat as fresh export
      return {
        providers: buildEavsProviders(
          req.providers,
          req.base_url,
          req.api_key
        ),
      };
    }

    const providers = existing.providers ?? {};

    // Remove all eavs-managed entries (anything prefixed with "eavs-")
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

    return { ...existing, providers };
  },
});
