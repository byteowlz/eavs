# eavs - a no-nonsense LLM proxy

A local, Rust-based LLM proxy with zero-latency bidirectional streaming, full logging, and live context injection.

## Features

- **Multi-Provider Support**: OpenAI, Anthropic, Google, Mistral, Groq, Cerebras, xAI, OpenRouter, Azure, and any OpenAI-compatible API (Ollama, vLLM, LM Studio)
- **Transparent Proxy**: Forwards requests with zero latency
- **Live Logging**: Multiple backends (stdout, file, webhook, OpenTelemetry)
- **Context Injection**: Pre-request injection of system or user messages
- **Conversation State**: TTL-based state management with automatic cleanup
- **Control API**: Manage injections, conversations, and stream logs in real-time

## Quick Start

```bash
# Set your API key
export OPENAI_API_KEY=your_key_here

# Run the server
cargo run

# Test with curl
curl http://localhost:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer unused" \
  -d '{"model": "gpt-4", "messages": [{"role": "user", "content": "Hello!"}]}'
```

## Configuration

EAVS uses TOML configuration. It looks for config files in:

1. `$XDG_CONFIG_HOME/eavs/config.toml` (or `~/.config/eavs/config.toml`)
2. `./config.toml` (current directory, overrides global)

See [`config/config.example.toml`](config/config.example.toml) for a fully documented example configuration. A JSON schema is available at [`config/config.schema.json`](config/config.schema.json) for editor validation and autocompletion.

### Providers

Configure multiple providers and select at runtime via `X-Provider` header:

```toml
[providers.default]
type = "openai"
api_key = "env:OPENAI_API_KEY"

[providers.anthropic]
type = "anthropic"
api_key = "env:ANTHROPIC_API_KEY"

[providers.local]
type = "ollama"
base_url = "http://localhost:11434/v1"
```

Supported providers:

- `openai` - OpenAI API
- `anthropic` - Anthropic Claude
- `google` - Google Gemini
- `mistral` - Mistral AI
- `groq` - Groq (fast inference)
- `cerebras` - Cerebras
- `xai` - xAI (Grok)
- `openrouter` - OpenRouter
- `azure` - Azure OpenAI
- `ollama`, `vllm`, `openai-compatible` - Local/compatible APIs

### Logging

Configure multiple logging backends:

```toml
[logging]
default = "stdout"

[[logging.backends]]
type = "stdout"
format = "json"  # or "pretty"

[[logging.backends]]
type = "file"
path = "./logs/eavs.jsonl"
rotate = "daily"

[[logging.backends]]
type = "webhook"
url = "https://your-service.com/logs"
headers = { Authorization = "env:LOG_API_KEY" }
batch_size = 100
flush_interval_secs = 5
```

### Conversation State

```toml
[state]
enabled = true
ttl_secs = 3600              # 1 hour TTL
cleanup_interval_secs = 60   # Cleanup every minute
max_conversations = 10000    # Max concurrent conversations
```

## API Reference

### Proxy Endpoints

All `/v1/*` requests are forwarded to the configured upstream provider.

```bash
# Use default provider
curl http://localhost:3000/v1/chat/completions ...

# Use specific provider
curl http://localhost:3000/v1/chat/completions \
  -H "X-Provider: anthropic" ...

# Track conversation
curl http://localhost:3000/v1/chat/completions \
  -H "X-Conversation-ID: my-session" ...
```

### Control API

#### Health Check

```bash
curl http://localhost:3000/health
```

#### List Providers

```bash
curl http://localhost:3000/providers
```

#### Inject Context

```bash
curl -X POST http://localhost:3000/inject/my-conversation \
  -H "Content-Type: application/json" \
  -d '{"messages": [{"role": "system", "content": "You are a pirate."}]}'
```

#### Clear Injections

```bash
curl -X POST http://localhost:3000/clear/my-conversation
```

#### List Conversations

```bash
curl http://localhost:3000/conversations
```

#### Get Conversation Stats

```bash
curl http://localhost:3000/conversations/stats
```

#### Get Conversation Details

```bash
curl http://localhost:3000/conversations/my-conversation
```

#### Update Conversation Metadata

```bash
curl -X PATCH http://localhost:3000/conversations/my-conversation \
  -H "Content-Type: application/json" \
  -d '{"provider": "anthropic", "tags": ["test"]}'
```

#### Stream Logs (SSE)

```bash
curl http://localhost:3000/logs/stream
```

## Running Tests

```bash
cargo test
```

## License

MIT
