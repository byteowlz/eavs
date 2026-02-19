# Agent Harness Config Research

This document catalogs the configuration formats for various AI agent harnesses
to inform the eavs adapter implementations.

## Codex CLI (~/.codex/config.yaml)

```yaml
model: codex-mini-latest
provider: openai
providers:
  openai:
    name: OpenAI
    baseURL: https://api.openai.com/v1
    envKey: OPENAI_API_KEY
  openrouter:
    name: OpenRouter
    baseURL: https://openrouter.ai/api/v1
    envKey: OPENROUTER_API_KEY
  azure:
    name: AzureOpenAI
    baseURL: https://YOUR_PROJECT_NAME.openai.azure.com/openai
    envKey: AZURE_OPENAI_API_KEY
  gemini:
    name: Gemini
    baseURL: https://generativelanguage.googleapis.com/v1beta/openai
    envKey: GEMINI_API_KEY
  ollama:
    name: Ollama
    baseURL: http://localhost:11434/v1
    envKey: OLLAMA_API_KEY
  # ... more providers
disableResponseStorage: false
flexMode: false
reasoningEffort: high
history:
  maxSize: 1000
  saveHistory: true
  sensitivePatterns: []
tools:
  shell:
    maxBytes: 10240
    maxLines: 256
mcp:
  exa:
    type: remote
    url: https://mcp.exa.ai/mcp
```

**Mapping to eavs:**
- Each eavs provider becomes a codex `providers` entry
- eavs base_url maps to codex `baseURL`
- Provider type mapping needed (openai, anthropic, etc.)

## Pi Agent (~/.pi/agent/models.json)

Already implemented in `adapters/pi/adapter.ts`.

```json
{
  "providers": {
    "eavs-openai": {
      "baseUrl": "http://127.0.0.1:3000/openai/v1",
      "api": "openai-responses",
      "apiKey": "eavs-xxx",
      "models": [...]
    }
  }
}
```

## Goose (~/.config/goose/config.yaml)

```yaml
AZURE_OPENAI_ENDPOINT: https://xxx.openai.azure.com/
LITELLM_BASE_PATH: v1/chat/completions
OLLAMA_HOST: localhost
AZURE_OPENAI_DEPLOYMENT_NAME: xxx
OPENAI_HOST: http://localhost:1234/v1
extensions:
  autovisualiser:
    enabled: false
    type: builtin
  git:
    enabled: false
    type: stdio
    cmd: uvx
    args: [mcp-server-git]
```

**Mapping to eavs:**
- Uses env vars for provider configuration
- Can set provider-specific env vars per provider
- MCP extensions are separate from providers

## Aider (.aider.conf.yml or ~/.aider.conf.yml)

Simple YAML format:

```yaml
model: gpt-4o
api_key: sk-xxx
base_url: http://localhost:3000/v1
edit_format: diff
auto_commits: true
```

Or command-line args stored in config.

## Claude Code (~/.claude/settings.json)

Minimal config - mostly uses API for provider management:

```json
{
  "mcpServers": {
    "context7": {
      "command": "npx",
      "args": ["@upstash/context7"]
    }
  }
}
```

Providers are configured through the Claude Code CLI/API, not static files.

## OpenCode

Based on hstry adapter analysis, OpenCode stores session data but provider
config is likely in a config file. Need to check source for format.
