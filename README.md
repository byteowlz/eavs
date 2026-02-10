# eavs - a no-nonsense LLM proxy

A local, Rust-based LLM proxy with zero-latency bidirectional streaming, full logging, and live context injection.

## Features

- **Multi-Provider Support**: OpenAI, Anthropic, Google, Mistral, Groq, Cerebras, xAI, OpenRouter, Microsoft Foundry, Azure, AWS Bedrock, and any OpenAI-compatible API (Ollama, vLLM, LM Studio)
- **Transparent Traffic Capture**: Automatically intercept LLM API calls from any app via mitmproxy integration
- **Virtual API Keys**: Issue keys with rate limits, budgets, and model restrictions
- **Cost Tracking**: Automatic token counting and cost calculation per key
- **Transparent Proxy**: Forwards requests with zero latency
- **Live Logging**: Multiple backends (stdout, file, webhook, OpenTelemetry)
- **Context Injection**: Pre-request injection of system or user messages
- **Conversation State**: TTL-based state management with automatic cleanup
- **Control API**: Manage injections, conversations, and stream logs in real-time

## Quick Start

```bash
# Set your API key
export OPENAI_API_KEY=your_key_here

# Run the server (foreground)
eavs serve

# Or run as a background service
eavs service start

# Test with curl
curl http://localhost:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -d '{"model": "gpt-4", "messages": [{"role": "user", "content": "Hello!"}]}'
```

## Configuration

EAVS uses TOML configuration. It looks for config files in:

1. `--config PATH` / `EAVS_CONFIG` (explicit config file)
2. `EAVS_*` environment variables (e.g. `EAVS_SERVER__PORT=3001`)
3. `./eavs.toml` (current directory, overrides global)
4. `$XDG_CONFIG_HOME/eavs/config.toml` (or `~/.config/eavs/config.toml`, auto-created on first run)

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

API key values support three resolution methods:

| Syntax | Source | Example |
|--------|--------|---------|
| `env:VAR_NAME` | Environment variable | `api_key = "env:OPENAI_API_KEY"` |
| `keychain:account` | System keychain | `api_key = "keychain:openai"` |
| literal value | Config file (plaintext) | `api_key = "sk-..."` |

The `keychain:` prefix reads from the OS-native credential store (macOS Keychain, libsecret on Linux, Windows Credential Manager). Store secrets with `eavs secret set <account>` and reference them in config. This keeps credentials out of plaintext files -- useful for sandboxed environments where agents should not have direct access to secrets.

Supported providers:

- `openai` - OpenAI API
- `openai-responses` - OpenAI Responses API (same keys, /v1/responses)
- `openai-codex` - OpenAI Codex/ChatGPT backend via OAuth
- `anthropic` - Anthropic Claude
- `google` - Google Gemini
- `google-vertex` - Google Vertex AI (ADC or API key, requires `gcp_project` + `gcp_location`)
- `google-gemini-cli` - Google Cloud Code Assist / Gemini CLI (OAuth, free tier)
- `github-copilot` - GitHub Copilot (device code OAuth, auto token exchange + dynamic base URL)
- `mistral` - Mistral AI
- `groq` - Groq (fast inference)
- `cerebras` - Cerebras
- `xai` - xAI (Grok)
- `openrouter` - OpenRouter
- `azure` - Azure OpenAI
- `bedrock` - AWS Bedrock (with SigV4 signing)
- `ollama`, `vllm`, `openai-compatible` - Local/compatible APIs
- `mock` - Mock provider for testing (no network calls)

When to use which OpenAI provider:
- `openai` for the Chat Completions API (`/v1/chat/completions`).
- `openai-responses` for the Responses API (`/v1/responses`).
- `openai-codex` for the Codex/ChatGPT backend with OAuth tokens.

Provider shortcuts (no client changes required):
- `eavs provider use <name>` sets a runtime default for the auto endpoint.
- `eavs provider clear` resets to the config default.
- State is stored in XDG state at `~/.local/state/eavs/state.toml` (or `$XDG_STATE_HOME`).

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

### Virtual API Keys

Issue API keys with rate limits, budgets, and model restrictions:

```toml
[keys]
enabled = true
database_path = "~/.eavs/keys.db"
master_key = "env:EAVS_MASTER_KEY"  # Required for admin operations
default_rpm_limit = 60              # Requests per minute
default_budget_usd = 10.0           # Budget in USD
```

Manage keys via CLI:

```bash
# Create a key with limits
eavs key create --name "dev-key" --rpm 100 --budget 50.0

# Create a key with model restrictions
eavs key create --name "gpt4-only" --models "gpt-4,gpt-4-turbo"

# List all keys
eavs key list

# Check usage
eavs key usage <key-id>

# Revoke a key
eavs key revoke <key-id>

# Bind a key to an OAuth user (Claude Code / Codex)
eavs key bind <key-id> --oauth-user "<user-id>"

# Clear the OAuth binding
eavs key bind <key-id> --clear
```

### AWS Bedrock

```toml
[providers.bedrock]
type = "bedrock"
aws_region = "env:AWS_REGION"
aws_access_key_id = "env:AWS_ACCESS_KEY_ID"
aws_secret_access_key = "env:AWS_SECRET_ACCESS_KEY"
# aws_session_token = "env:AWS_SESSION_TOKEN"  # Optional
```

### Transparent Traffic Capture

Automatically intercept LLM API calls from any application (including desktop apps like ChatGPT and Claude) without changing client configuration. This feature uses mitmproxy for cross-platform traffic interception.

**Prerequisites:**
- mitmproxy 10.1.5+ (`brew install mitmproxy` on macOS, `pip install mitmproxy` elsewhere)
- On first run, trust mitmproxy's CA certificate (see [mitmproxy docs](https://docs.mitmproxy.org/stable/concepts/certificates/))

**Option 1: Auto-start via config (recommended)**

```toml
[capture]
enabled = true
mode = "local"              # Capture all local traffic
# mode = "local:ChatGPT"    # Capture specific app only
# verbose = true            # Enable verbose logging
# api_only = true           # Skip desktop app domains
```

Then just run `eavs serve` - mitmproxy starts automatically.

**Option 2: Manual mitmproxy start**

```bash
# Terminal 1: Start Eaves
eavs serve

# Terminal 2: Start mitmproxy with capture addon
mitmproxy --mode local -s scripts/eavs_capture.py

# Capture specific app only
mitmproxy --mode local:ChatGPT -s scripts/eavs_capture.py
```

**Captured domains:**
- API endpoints: api.openai.com, api.anthropic.com, generativelanguage.googleapis.com, api.mistral.ai, api.groq.com, etc.
- Desktop apps: chat.openai.com, claude.ai, gemini.google.com, perplexity.ai, poe.com, etc.

**How it works:**
1. mitmproxy intercepts outgoing HTTPS traffic using its local capture mode
2. The `eavs_capture.py` addon detects LLM-related domains and redirects them to Eaves
3. Eaves auto-detects the provider from the original host and proxies the request
4. All traffic is logged and can be analyzed, rate-limited, or modified

## CLI Reference

### Service Management

```bash
# Start server in foreground
eavs serve --host 0.0.0.0 --port 8080

# Background service
eavs service start [--port 3000]
eavs service stop
eavs service restart
eavs service status
eavs service logs

# Manage secrets in the system keychain
eavs secret set openai                # Store interactively (hidden input)
eavs secret set anthropic --value sk-ant-...
eavs secret get openai                # Show masked value
eavs secret get openai --reveal       # Show full value
eavs secret delete openai             # Remove from keychain
eavs secret list --check              # List keychain refs from config + check availability
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

## Testing and Benchmarking

```bash
# Run tests
cargo test

# Quick chat test
eavs test chat "Hello" --provider openai

# Sequential benchmark (mock provider = no API costs)
eavs test bench --provider mock --count 50

# Concurrent benchmark
eavs test bench --provider mock --concurrent 10 --count 100

# Duration-based load test
eavs test bench --provider mock --concurrent 50 --duration 30s
```

### Benchmark Options

| Flag | Description |
|------|-------------|
| `--provider <name>` | Provider to test (default: "default") |
| `--count <n>` | Number of requests (default: 10) |
| `--concurrent <n>` | Parallel requests (default: 1) |
| `--duration <time>` | Run for duration (e.g., "30s", "1m") |
| `--model <model>` | Model to use (optional) |

The `mock` provider returns synthetic responses without network calls, ideal for measuring proxy overhead.

## License

MIT
