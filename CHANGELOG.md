# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.0] - 2026-10-28

### Added
- Subscription and credit monitoring: eavs reads each upstream account's own
  usage endpoint every 5 minutes (no inference) and serves the cached result at
  `GET /admin/usage/subscriptions` (`?refresh=true` reads due accounts now).
  Covers Claude subscription logins (session/weekly/model windows, extra usage
  against the monthly cap), Codex/ChatGPT logins (windows, credits), OpenCode Go
  (session/weekly/monthly), OpenRouter (credit balance, key cap) and Z.ai GLM
  Coding Plan (windows, web search). Unknown windows are absent, never zero;
  rate limits keep the last good values and honour `retry-after`. New Usage view
  in the web console.
- Web console Providers view: add, inspect, test, delete runtime providers and
  manage their models and keys; providers added at runtime appear in the
  availability view and are probed when they opt in.
- Storage follows XDG. The key store, providers database and SQLite login
  store default to `$XDG_DATA_HOME/eavs/` (`~/.local/share/eavs/`) instead of
  `~/.eavs/`; on start the server moves an existing `~/.eavs` store there once
  (never overwriting) and makes it owner-only (files 0600, directory 0700;
  they hold provider credentials). An explicit `keys.database_path` is left
  alone; the old default value counts as the default. The machine-bound device
  registry moves from `$XDG_CONFIG_HOME/eavs/device.json` to
  `$XDG_STATE_HOME/eavs/device.json`, out of the synced config directory.
- `eavs device claude-code --name <connection> [--provider claude]`: point
  Claude Code on this machine at the gateway's Claude subscription. Previews,
  then applies with the printed revisions; merges `ANTHROPIC_BASE_URL`,
  `CLAUDE_CODE_OAUTH_TOKEN` (the connection's scoped key; `--no-store-key` to
  export it from the shell instead) and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`
  into `~/.claude/settings.json`, removes `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN`
  there, keeps everything else and makes the file owner-only. `--remove` undoes it.
- ADR-0022: native GPUI console over the per-instance operator API.
- Native console (`ui/native`, `just console`): manage the gateway on this
  machine and remote gateways (e.g. the server over an SSH tunnel or the
  tailnet) from one window. Overview with what runs out first, providers with
  presets ("pick it, paste a key"), model availability switches, and
  subscription/credit usage meters with reset countdowns. Credentials are
  `env:`/`keychain:` references; the local gateway is found from eavs' config.

### Changed
- Claude subscriptions (ADR-0023): an Anthropic provider holding a subscription
  token (`sk-ant-oat…`, e.g. from `claude setup-token`) or a key bound to an
  Anthropic OAuth login now serves only the genuine Claude Code client, passed
  through byte for byte with only the credential swapped. Point Claude Code at
  it with `ANTHROPIC_BASE_URL=https://<gateway>/<provider>`,
  `CLAUDE_CODE_OAUTH_TOKEN=<eavs key>` and
  `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` (without it Claude Code sends the
  token straight to Anthropic for telemetry and bootstrap). Other clients get
  `403 subscription_requires_claude_code`. **Breaking:** the previous
  identity-injection path for non-Claude-Code clients is removed.
- `/v1/messages/count_tokens` is routed for native Anthropic clients.
- OAuth token prefixes are no longer logged.
- A provider's model shortlist is its probe universe: configured models the
  runtime does not list are marked unavailable.

## [0.9.2] - 2026-10-25

### Fixed
- Admin console data views: the listener token unlocks the gateway boundary, but
  keys and accounts handlers also need the `master_key` (Bearer). The gate now
  takes both credentials and sends them together; availability still accepts a
  maintenance-only key alone. Verified live on the Mac: availability, keys and
  accounts all answer 200 with the pair.

## [0.9.1] - 2026-10-25

### Added
- Admin console: an installable PWA at `/admin`. Provider/model overview with
  live availability and probe status (click a model to enable/disable), virtual
  key management, and OAuth account status. The shell is static, data flows
  only through authenticated calls (operator token or maintenance key).
- Local-runtime availability probe: `[providers.*.probe] enabled = true` makes
  EAVS periodically ask a self-hosted runtime (llama.cpp, vLLM, ollama,
  llama-swap) for its served model list — a plain `GET /models`, never a
  generation — and deactivate configured models the runtime no longer serves,
  with the reason and probe time in the snapshot. Operator deactivations are
  never overridden by probes. Probe results appear in
  `GET /eavs/maintenance/availability` alongside each provider's configured
  model universe.
- Model availability maintenance: deactivate models without touching config, e.g.
  when a GPU machine switches loadouts. `POST /eavs/maintenance/availability`
  accepts the operator token or a new maintenance-only key (`eavs keys create
  --maintenance`; such keys are rejected on every inference route) and supports
  targeted updates plus `mode: "replace"` to announce a provider's complete
  active set, with optional reason and TTL. Deactivated models vanish from
  catalogs and direct requests answer a typed `model_unavailable` error naming
  the reason and remedy. CLI: `eavs models availability|disable|enable`.
- Per-model native API: models carry `api` (`openai-completions`, `openai-responses`,
  `anthropic-messages`), from config or, on gateways serving every format (OpenCode,
  OpenCode Go), from models.dev's per-model SDK. The serving catalog publishes `api` and
  `request_path` per model when it differs from the provider's, so clients use e.g.
  Anthropic Messages for Qwen3.8/MiniMax and Responses for GPT on OpenCode Go.
- models.dev `reasoning_options` effort values seed accepted reasoning efforts (after
  config and learned values); `max` is a recognized effort level. The models.dev cache
  file is now `models_catalog.v2.json` so these fields are fetched immediately.
- Session affinity for every provider: the client's conversation id (`x-session-affinity`,
  `x-opencode-session`, `x-session-id`, `session_id` or `X-Conversation-ID`) is sent
  upstream as an opaque id scoped to the virtual key and provider, in the provider's
  header (`x-opencode-session` for OpenCode, `x-session-id` for OpenRouter; configurable
  via `session_header`). Without one, the id is stable per key; previously OpenCode got
  the literal `default` for every conversation of every key.
- Upstream User-Agent: previously none was sent. Default is `<client UA> eavs/<version>`;
  configurable per provider via `user_agent` with `{client}` and `{eavs}`.
- Anthropic adaptive thinking: models that reject `thinking.type: "enabled"` (e.g. Claude
  Sonnet 5.5) get `{type: "adaptive"}` plus `output_config.effort` derived from the token
  budget. Learned from the first upstream 400 (persisted, one retry) or declared via model
  compat `forceAdaptiveThinking = true`; published in the catalog's model `compat`.
  Reported via `x-eavs-thinking: enabled->adaptive(<effort>)`.
- FastAPI-style upstream errors (`{"detail": ...}`) keep their message when normalized.

### Changed
- **Breaking:** an unprefixed `/v1` request whose model name suggests a provider kind
  (e.g. `deepseek-*`) is routed only to a provider of that name, or to the single
  configured provider of that type (so `[providers.default] type = "openai"` still
  serves `gpt-*`). Otherwise it returns 400 `unknown_provider` naming the guess and the
  remedy instead of silently falling back to `default`. Clients relying on the old
  fallback should send `X-Provider` or use `/<provider>/v1/...` (eavs-achj).
- Unknown providers (path or `X-Provider`) return 400 `unknown_provider` and no longer
  list every configured provider to any caller.
- Non-inference provider paths return a JSON 404 `unknown_endpoint` naming the expected
  endpoints instead of an empty 404.
- These errors add "Providers available to this key: ..." for a valid virtual key,
  computed like the key-filtered catalog; unauthenticated callers get no list.

## [0.9.0] - 2026-10-22

### Added
- Reasoning-effort normalization (eavs-5a8d): per-model `reasoning_efforts`; unsupported
  `reasoning_effort` / `reasoning.effort` values are rewritten to the nearest accepted
  level (preferring higher). Undeclared models learn the accepted set from the first
  upstream 400 that names it (persisted, one retry). Published in `/v1/eavs/catalog`;
  rewrites are reported via `x-eavs-reasoning-effort`.
- Scoped device onboarding, native Pi streaming, administration clients and an Oqto Pi
  invitation command with private QR publication.
- Opt-in CORS and a scoped native inference listener.
- Realtime speech, tenant-bound async jobs, rerank bridge and native inference HTTP
  shapes with provider-scoped auth; metering for budgeted jobs and Cohere/Voyage rerank.
- OpenCode and OpenCode Go providers; modernized OpenAI Responses/Codex request shaping.

### Changed
- Unprefixed `/v1` requests whose model name suggests an unconfigured provider (and no
  default exists) now return 400 `unknown_provider` naming the guess and the
  `/<provider>/v1` / `X-Provider` remedies (eavs-achj).
- Authoritative live key admission across processes.

### Fixed
- Bounded upstream error-body reads so slow or unterminated errors never hang.
- Security audit findings across proxy, keys, OAuth, probe and egress; OAuth ownership
  and effective model scope guards; delegated media and native budget gates.

## [0.8.3] - 2026-08-27

### Security
- Strip provider-delegated remote file fetches and hosted tools from HTTP,
  Realtime WebSocket, and Codex WebSocket request bodies by default. Explicit
  configuration can allow remote content or individual hosted tool types;
  virtual-key metadata provides a tenant-scoped remote-content override. Every
  removal emits a structured audit event.

## [0.8.2] - 2026-08-25

### Security
- Require a capability token on every HTTP/WebSocket listener route, including
  health and provider metadata. Unauthenticated requests now receive `401`
  without disclosing configured providers. Fresh installations generate a
  256-bit listener token in the user-only configuration file.

## [0.8.1] - 2026-07-28

### Fixed
- `compat` settings are now applied to pass-through `/v1/chat/completions`
  requests. Previously `supports_developer_role`, `supports_store`,
  `supports_stream_options`, and `max_tokens_field` were only honoured on the
  canonical transform path (Anthropic/Google/Bedrock/Responses), so they were
  silently ignored for every `openai-compatible` provider — the exact providers
  the settings exist for. A client sending the `developer` role to an upstream
  that only accepts `system` got a 422 despite
  `supports_developer_role = false`. Only explicitly configured or URL-detected
  settings are applied; unset fields leave the body untouched.

## [0.7.0] - 2026-06-10

### Added
- **Egress Firewall** (`[egress]`): a transparent egress proxy for front ends
  that redirect TCP traffic and announce the original destination via a PROXY
  protocol v2 header. Recovers the destination, derives the hostname from TLS
  SNI / HTTP Host, enforces the `[network]` domain ACL (enforce or monitor
  posture), and splices. Includes a DNS relay. Front-end agnostic; disabled by
  default.

### Changed
- **Logging is now content-safe by default.** Request/response bodies (prompts,
  `messages`, model output) are no longer logged unless `logging.log_bodies =
  true`. Previously JSON/file backends serialized full conversation content.
  Redaction happens before any sink, so no backend sees content when disabled.

## [0.5.10] - 2026-02-19

### Added
- `eavs models configured` command to show models from eavs.toml shortlists
- `--discover` flag to probe provider endpoints for available models
- Model discovery for OpenAI-compatible, Google Gemini, and Ollama endpoints
- Non-chat model filtering (removes fine-tunes, embeddings, TTS, image, realtime models)
- JSON output support via `--json` flag
- Optional provider filter argument

## [0.5.3] - 2026-02-17

### Added
- Release infrastructure for automated GitHub releases, Homebrew, and AUR publishing
- Build binaries for Linux x86_64/ARM64 and macOS Intel/Apple Silicon
- Homebrew formula in byteowlz/homebrew-tap with automatic updates
- AUR package with automatic PKGBUILD updates
- Just recipes: `release`, `release-check`, `changelog` for automated release workflow
- Comprehensive release documentation in `docs/RELEASE.md`

### Changed
- Updated README with installation instructions for Homebrew, AUR, Cargo, and pre-built binaries
- Added `test_model` field to provider config for specifying the model used by `eavs setup test` and `eavs setup test-all`
- Added automatic test model discovery from `~/.pi/agent/models.json` (matched by base_url) as a fallback when `test_model` is not set in config
- Test model resolution precedence: config `test_model` > pi models.json > built-in defaults
- Azure AI Foundry configuration now uses standard provider types (openai, anthropic, openai-compatible) instead of a dedicated "foundry" provider type. This gives users more control over API format selection based on the model they're using (GPT models use OpenAI format, Claude models use Anthropic format, etc.)

### Fixed
- Fixed `eavs setup test-all` summary incorrectly counting failed tests as passed when user selects "Continue anyway?" (e.g., showing "14/14 passed" when only 3 actually passed)

### Removed
- Removed dedicated `foundry` provider type. Users should now configure Azure AI Foundry using the appropriate provider type for their model (see config.example.toml for examples)

## [0.5.13] - 2026-02-25

### Added
- Enhanced mock provider with 12 configurable test scenarios: simple_text, tool_call, multi_tool, tool_call_then_text, error_mid_stream, rate_limit, server_error, timeout, connection_reset, thinking, long_text, malformed_sse
- Scenario selection via `X-Mock-Scenario` request header or model name (e.g., `mock/tool-call`)
- Configurable per-chunk streaming delay via `X-Mock-Delay-Ms` header (default: 30ms)
- Mock scenario models registered in `/providers/detail` for models.json generation
- Auto-routing for `mock/*` model names to mock provider
- Full chunk audit logging with timestamps for mock responses
- `/catalog/lookup` endpoint for model metadata from models.dev
- Provider detail endpoint now exposes headers, api_version, and compat settings

## [Unreleased]

## [0.10.0] - 2026-10-28

## [0.6.1] - 2026-04-09

### Fixed
- Dynamic mock responses now work with array-style message content (OpenAI format with `content: [{type: "text", text: "..."}]`), fixing compatibility with TUI clients

### Added
- Provider templates for quick provider setup (`eavs setup` with templates)
- `eavs secret list --all` to show all stored secrets
- Dynamic mock mode with predefined responses from config (`[mock_responses.<name>]`)

### Changed
- Security: disabled sqlx default features to remove unused mysql/rsa dependencies
