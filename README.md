![banner](banner.png)

# eavs - a no-nonsense LLM proxy

A local, Rust-based LLM proxy with zero-latency bidirectional streaming, full logging, and live context injection.

## Features

- **Multi-Provider Support**: OpenAI, Anthropic, Google, Mistral, Groq, Cerebras, xAI, OpenRouter, Microsoft Foundry, Azure, AWS Bedrock, and any OpenAI-compatible API (Ollama, vLLM, LM Studio)
- **OAuth Authentication**: Anthropic, OpenAI Codex, Google Gemini CLI, GitHub Copilot -- with multi-account support per provider
- **WebSocket Proxy**: OpenAI Realtime API and Codex Responses WebSocket transport
- **Policy Engine**: Request/response rewriting rules (e.g., force `store: true` for Codex models)
- **Network Access Control**: Domain allow/deny lists with private IP blocking (SSRF prevention)
- **Upstream Quota Tracking**: Parse rate limit headers from providers, surface via API and CLI
- **Model Catalog**: Browse 2800+ models across 90+ providers via [models.dev](https://models.dev/) integration
- **Transparent Traffic Capture**: Automatically intercept LLM API calls from any app via mitmproxy integration
- **Virtual API Keys**: Issue keys with rate limits, budgets, model restrictions, and OAuth account binding
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

## Installation

### Homebrew (macOS and Linux)

```bash
brew tap byteowlz/tap
brew install eavs
```

### Arch Linux (AUR)

```bash
# Using yay (recommended)
yay -S eavs

# Using paru
paru -S eavs

# Using makepkg (manual)
git clone https://aur.archlinux.org/eavs.git
cd eavs
makepkg -si
```

### Cargo

```bash
cargo install eavs
```

### Pre-built Binaries

Download pre-built binaries from the [GitHub Releases](https://github.com/byteowlz/eavs/releases) page.

Available platforms:

- Linux x86_64 and ARM64
- macOS Intel and Apple Silicon

### Build from Source

```bash
git clone https://github.com/byteowlz/eavs.git
cd eavs
cargo build --release
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

| Syntax             | Source                  | Example                          |
| ------------------ | ----------------------- | -------------------------------- |
| `env:VAR_NAME`     | Environment variable    | `api_key = "env:OPENAI_API_KEY"` |
| `keychain:account` | System keychain         | `api_key = "keychain:openai"`    |
| literal value      | Config file (plaintext) | `api_key = "sk-..."`             |

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

### Interactive Setup

Add providers interactively with guided prompts for API keys, base URLs, and provider-specific settings. The wizard supports all credential storage methods (literal, `env:`, `keychain:`).

```bash
# Interactive wizard -- walks through provider type, credentials, and base URL
eavs setup add

# Test a single provider directly (no server needed)
eavs setup test anthropic
eavs setup test foundry-openai --model gpt-4o

# Test all configured providers in one shot
eavs setup test-all

# Show resolved config for a provider (env vars expanded, defaults applied)
eavs setup show anthropic
eavs setup show anthropic --reveal   # show unmasked API key
```

The `eavs setup test` command bypasses the proxy and makes a direct API call to the upstream provider. This is useful for validating credentials, base URLs, and connectivity before starting the server.

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

### OAuth Authentication

Authenticate with providers that use OAuth instead of static API keys:

```bash
# Interactive login (shows provider selection menu)
eavs login

# Login to a specific provider
eavs login anthropic
eavs login openai        # OpenAI Codex (ChatGPT Pro)
eavs login google         # Google Gemini CLI
eavs login github-copilot # GitHub Copilot

# Login with a specific account label (multi-account)
eavs login openai --user default --account pro-subscription

# Check OAuth status
eavs auth status
```

Virtual keys can be bound to OAuth users and specific accounts:

```bash
# Create key bound to OAuth user + account
eavs key create --name "codex-pro" --oauth-user "default" --oauth-account "pro-subscription"
```

### Policy Rules

Rewrite request fields before they reach the upstream provider:

```toml
[[policy.rules]]
action = "set_field"
provider = "openai*"
model = "gpt-5*"
path = "store"
value = true  # Force store=true for Codex models (fixes Pi 404 errors)
```

Policy rules support glob matching on provider and model names.

### Network Access Control

Restrict which upstream domains the proxy can connect to:

```toml
[network]
# Only allow these upstream domains (empty = allow all)
allow_domains = ["api.openai.com", "*.anthropic.com", "api.groq.com"]

# Block these domains (checked before allow list)
deny_domains = ["*.internal.corp", "evil.com"]

# Block private IPs to prevent SSRF (default: true)
block_private_ips = true
```

Precedence: deny list > private IP check > allow list. Empty lists impose no restriction.

### Upstream Quota Tracking

Rate limit headers from upstream providers are automatically parsed and tracked:

```bash
# View current quotas
eavs quotas

# JSON output
eavs quotas --json
```

Supported headers:

- OpenAI: `x-ratelimit-limit-requests`, `x-ratelimit-remaining-requests`, etc.
- Anthropic: `anthropic-ratelimit-requests-limit`, `anthropic-ratelimit-requests-remaining`, etc.

Quotas are also available via the admin API: `GET /admin/quotas`.

### Model Catalog

Browse models from [models.dev](https://models.dev/) (2800+ models, 90+ providers):

```bash
# List models for a provider
eavs models list openai
eavs models list anthropic --json

# Search across all providers
eavs models search "codex"
eavs models search "sonnet" --json

# Update cached catalog
eavs models update

# Show catalog stats
eavs models stats
```

Providers can define a model shortlist in config to curate which models are available:

```toml
[[providers.openai.models]]
id = "gpt-5.2-codex"
name = "GPT-5.2 Codex"

[[providers.openai.models]]
id = "o3"
name = "o3"
```

When a shortlist is defined, only those models appear. When empty, the full models.dev catalog is used.

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
```

### Keys

```bash
eavs key create --name "dev" --rpm 100 --budget 50.0
eavs key create --name "codex" --oauth-user default --oauth-account pro
eavs key list
eavs key usage <key-id>
eavs key revoke <key-id>
eavs key bind <key-id> --oauth-user "<user-id>"
```

### OAuth

```bash
eavs login [provider]               # Interactive OAuth login
eavs login openai --account second   # Multi-account login
eavs auth status                     # Show OAuth credential status
```

### Models

```bash
eavs models list <provider>          # List models for a provider
eavs models list openai --json       # JSON output
eavs models search <query>           # Search across all providers
eavs models search "codex" --json    # JSON output
eavs models update                   # Refresh catalog from models.dev
eavs models stats                    # Show catalog statistics
```

### Quotas

```bash
eavs quotas                          # Show upstream rate limit quotas
eavs quotas --json                   # JSON output
```

### Secrets

```bash
eavs secret set openai                # Store interactively (hidden input)
eavs secret set anthropic --value sk-ant-...
eavs secret get openai                # Show masked value
eavs secret get openai --reveal       # Show full value
eavs secret delete openai             # Remove from keychain
eavs secret list --check              # List keychain refs from config + check availability
```

### Providers

```bash
eavs provider list                   # List configured providers
eavs provider use <name>             # Set runtime default
eavs provider clear                  # Reset to config default
eavs setup add                       # Interactive provider setup
eavs setup test <provider>           # Test a provider directly
eavs setup test-all                  # Test all providers
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

#### Provider Detail (for integrations)

```bash
# Get all providers with models, pricing, and Pi API mapping
curl http://localhost:3000/providers/detail \
  -H "Authorization: Bearer $EAVS_MASTER_KEY"
```

#### Admin: Upstream Quotas

```bash
curl http://localhost:3000/admin/quotas \
  -H "Authorization: Bearer $EAVS_MASTER_KEY"
```

### WebSocket Endpoints

#### OpenAI Realtime API

```
ws://localhost:3000/v1/realtime?model=gpt-4o-realtime-preview
ws://localhost:3000/<provider>/v1/realtime?model=gpt-4o-realtime-preview
```

#### Codex Responses (WebSocket transport)

```
ws://localhost:3000/v1/codex/responses
ws://localhost:3000/<provider>/v1/codex/responses
```

The Codex WebSocket proxy intercepts `response.create` messages for policy application (e.g., `store: true`) and tracks usage from `response.completed` events.

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

| Flag                | Description                           |
| ------------------- | ------------------------------------- |
| `--provider <name>` | Provider to test (default: "default") |
| `--count <n>`       | Number of requests (default: 10)      |
| `--concurrent <n>`  | Parallel requests (default: 1)        |
| `--duration <time>` | Run for duration (e.g., "30s", "1m")  |
| `--model <model>`   | Model to use (optional)               |

The `mock` provider returns synthetic responses without network calls, ideal for measuring proxy overhead.

## Contributing

Contributions are welcome! Please see [docs/RELEASE.md](docs/RELEASE.md) for information about the release process.

## Release Process

The release process is fully automated via GitHub Actions:

1. **GitHub Releases**: Automatic builds for Linux (x86_64/ARM64) and macOS (Intel/Apple Silicon)
2. **Homebrew**: Automatic formula updates in `byteowlz/homebrew-tap`
3. **AUR**: Automatic PKGBUILD updates

See [docs/RELEASE.md](docs/RELEASE.md) for detailed release instructions.

## Acknowledgements

- Model catalog data provided by [models.dev](https://models.dev/) -- a comprehensive, community-maintained database of LLM model metadata, pricing, and capabilities.
- Provider-specific API quirks and protocol handling informed by [pi-mono](https://github.com/badlogic/pi-mono) -- the monorepo behind the Pi coding agent.

## License

MIT
