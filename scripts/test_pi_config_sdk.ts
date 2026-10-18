// Credential-blind Pi models.json parser + synthetic interpolation proof. No inference.
import assert from "node:assert/strict";
import { resolve } from "node:path";
const [source, fixture] = process.argv.slice(2);
if (!source || !fixture) throw new Error("usage: bun scripts/test_pi_config_sdk.ts PI_MONO SYNTHETIC_MODELS_JSON");
const installed = await Bun.file(resolve(source, "dist/core/model-config.js")).exists();
const core = installed ? "dist/core" : "packages/coding-agent/src/core";
const extension = installed ? "js" : "ts";
const { ModelConfig } = await import(resolve(source, core, `model-config.${extension}`));
const { getConfigValueEnvVarNames, isCommandConfigValue, resolveConfigValueOrThrow } = await import(resolve(source, core, `resolve-config-value.${extension}`));
const config = await ModelConfig.load(resolve(fixture));
assert.equal(config.getError(), undefined, "actual Pi config parser must accept the generated document");
let providers = 0;
for (const id of config.getProviderIds()) {
  if (!id.startsWith("eavs-device-")) continue; // Never resolve unrelated credentials/commands.
  const provider = config.getProvider(id);
  assert.ok(provider && provider.models?.length);
  assert.ok(["openai-completions", "openai-responses", "anthropic-messages"].includes(provider.api));
  assert.ok(!isCommandConfigValue(provider.apiKey));
  const env = Object.fromEntries(getConfigValueEnvVarNames(provider.apiKey).map((name: string) => [name, "synthetic-scoped-key"]));
  assert.equal(resolveConfigValueOrThrow(provider.apiKey, "fixture key", env), "synthetic-scoped-key");
  assert.equal(resolveConfigValueOrThrow(provider.headers.Authorization, "fixture bearer", env), "Bearer synthetic-scoped-key");
  if (provider.headers["X-Eavs-Token"]) {
    const listener = provider.headers["X-Eavs-Token"];
    assert.ok(!isCommandConfigValue(listener));
    const local = Object.fromEntries(getConfigValueEnvVarNames(listener).map((name: string) => [name, "synthetic-listener-key"]));
    assert.equal(resolveConfigValueOrThrow(listener, "fixture listener", local), "synthetic-listener-key");
  }
  providers++;
}
assert.ok(providers > 0);
console.log(`Actual Pi config parser/interpolation: ${providers} scoped provider(s) pass; no commands, credentials or inference.`);
