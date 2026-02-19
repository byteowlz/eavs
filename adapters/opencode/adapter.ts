/**
 * OpenCode adapter for eavs model export
 *
 * Generates OpenCode-compatible opencode.json from eavs provider configuration.
 * OpenCode stores config in ~/.config/opencode/opencode.json
 *
 * OpenCode uses AI SDK providers with npm packages and an options object
 * containing apiKey and baseURL.
 */

import type {
  AdapterInfo,
  ExportRequest,
  MergeRequest,
  EavsProvider,
} from "../types/index.ts";
import { runAdapter } from "../types/index.ts";

const MANAGED_PREFIX = "eavs-";

/** Map eavs provider type to OpenCode npm package */
function opencodeNpmPackage(providerType: string): string {
  const packages: Record<string, string> = {
    openai: "@ai-sdk/openai",
    anthropic: "@ai-sdk/anthropic",
    google: "@ai-sdk/google",
    "google-vertex": "@ai-sdk/google-vertex",
    azure: "@ai-sdk/azure",
    mistral: "@ai-sdk/mistral",
    groq: "@ai-sdk/groq",
    cerebras: "@ai-sdk/cerebras",
    xai: "@ai-sdk/xai",
    openrouter: "@ai-sdk/openai-compatible",
    ollama: "@ai-sdk/openai-compatible",
    bedrock: "@ai-sdk/amazon-bedrock",
    "openai-compatible": "@ai-sdk/openai-compatible",
  };

  return packages[providerType] ?? "@ai-sdk/openai-compatible";
}

/** Build OpenCode provider entry */
function buildOpencodeProvider(
  provider: EavsProvider,
  baseUrl: string,
  apiKey: string
): Record<string, unknown> | null {
  const name = provider.name;
  const type = provider.type;

  // Skip default provider - will be handled by selecting the first available
  if (name === "default") return null;

  const npm = opencodeNpmPackage(type);

  // Build models object
  const models: Record<string, unknown> = {};
  for (const model of provider.models) {
    const modelEntry: Record<string, unknown> = {
      name: model.name || model.id,
    };

    // Add limits if available
    if (model.context_window || model.max_tokens) {
      modelEntry.limit = {
        context: model.context_window || 128000,
        output: model.max_tokens || 4096,
      };
    }

    models[model.id] = modelEntry;
  }

  // If no models in shortlist, add a placeholder
  if (Object.keys(models).length === 0) {
    models["default-model"] = { name: "Default Model" };
  }

  // Build eavs-specific provider name
  const providerKey = `${MANAGED_PREFIX}${name}`;

  return {
    [providerKey]: {
      models,
      name: name.charAt(0).toUpperCase() + name.slice(1).replace(/[_-]/g, " "),
      npm,
      options: {
        apiKey: apiKey,
        baseURL: `${baseUrl.replace(/\/$/, "")}/${name}/v1`,
      },
    },
  };
}

/** Build complete OpenCode config */
function buildOpencodeConfig(
  providers: EavsProvider[],
  baseUrl: string,
  apiKey: string
): Record<string, unknown> {
  const providerEntries: Record<string, unknown> = {};

  for (const provider of providers) {
    const entry = buildOpencodeProvider(provider, baseUrl, apiKey);
    if (entry) {
      Object.assign(providerEntries, entry);
    }
  }

  // Determine default model
  let defaultModel = "anthropic/claude-sonnet_4";
  const firstProvider = providers.find((p) => p.name !== "default");
  if (firstProvider && firstProvider.models.length > 0) {
    const firstModel = firstProvider.models[0];
    defaultModel = `${MANAGED_PREFIX}${firstProvider.name}/${firstModel.id}`;
  }

  return {
    $schema: "https://opencode.ai/config.json",
    autoshare: false,
    autoupdate: true,
    model: defaultModel,
    provider: providerEntries,
    share: "disabled",
    theme: "default",
    // Empty MCP and mode sections for user to fill in
    mcp: {},
    mode: {
      build: {},
      document: {},
      journal: {},
      plan: {},
    },
  };
}

runAdapter({
  info(): AdapterInfo {
    return {
      name: "opencode",
      displayName: "OpenCode",
      version: "1.0.0",
      outputFile: "opencode.json",
      defaultPath: "~/.config/opencode/opencode.json",
      description: "Generates ~/.config/opencode/opencode.json for the OpenCode IDE",
      managedPrefix: MANAGED_PREFIX,
    };
  },

  export(req: ExportRequest): Record<string, unknown> {
    return buildOpencodeConfig(req.providers, req.base_url, req.api_key);
  },

  merge(req: MergeRequest): Record<string, unknown> {
    // Parse existing config
    let existing: Record<string, unknown>;
    try {
      existing = JSON.parse(req.existing);
    } catch {
      return buildOpencodeConfig(req.providers, req.base_url, req.api_key);
    }

    // Get existing providers
    const existingProviders =
      (existing.provider as Record<string, unknown>) ?? {};

    // Remove eavs-managed providers
    for (const key of Object.keys(existingProviders)) {
      if (key.startsWith(MANAGED_PREFIX)) {
        delete existingProviders[key];
      }
    }

    // Build new eavs providers
    const eavsProviders: Record<string, unknown> = {};
    for (const provider of req.providers) {
      const entry = buildOpencodeProvider(
        provider,
        req.base_url,
        req.api_key
      );
      if (entry) {
        Object.assign(eavsProviders, entry);
      }
    }

    // Merge providers
    existing.provider = {
      ...existingProviders,
      ...eavsProviders,
    };

    // Update model reference if it was pointing to a removed eavs provider
    const currentModel = existing.model as string;
    if (currentModel && currentModel.startsWith(MANAGED_PREFIX)) {
      const firstEavsKey = Object.keys(eavsProviders)[0];
      if (firstEavsKey) {
        const firstProvider = eavsProviders[firstEavsKey] as {
          models?: Record<string, unknown>;
        };
        const firstModel = Object.keys(firstProvider?.models ?? {})[0];
        if (firstModel) {
          existing.model = `${firstEavsKey}/${firstModel}`;
        }
      }
    }

    return existing;
  },
});
