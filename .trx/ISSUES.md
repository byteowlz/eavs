# Issues

## Open

### [eavs-qcfn.3] Model alias resolution in proxy -- rewrite default/fast/reasoning to real model IDs (P1, task)
When the proxy receives a request with model field set to a named alias ("default", "fast", "reasoning", "fallback"), OR with model field missing/empty/null, resolve it to the actual model ID from [defaults] config before routing.

Resolution order:
1. model field is a known alias ("default", "fast", "reasoning", "fallback") -> resolve to configured model
2. model field is missing, empty, or null -> resolve to "default" tier
...


### [eavs-qcfn.2] Implement GET /defaults endpoint (P1, task)
New API endpoint that returns the resolved model defaults.

GET /defaults returns:
```json
{
...


### [eavs-qcfn.1] Add [defaults] config section for named model tiers (P1, task)
Add a new [defaults] section to eavs config.toml that maps named tiers to specific models:

```toml
[defaults]
default = "claude-sonnet-4"
...


### [eavs-sth0] Secret-aware API proxying: extend eavs to resolve kyz secret references and inject credentials at transport layer (P1, feature)
## Summary

~~Extend eavs with credential proxying~~ -- REVISED: the proxy endpoint belongs on **oqto-runner**, not eavs.

Eavs is a shared central service. kyz vaults are per-user. The runner already runs as the target user and has access to their kyz vault. The credential proxy naturally lives on the runner.
...


### [eavs-1sfs] Upstream 401 errors cause request to hang instead of returning error (P1, bug)

### [eavs-qcfn.6] CLI: eavs ask -- one-shot LLM queries for scripts (P2, feature)
Add an 'eavs ask' subcommand for quick one-shot LLM calls from the command line and shell scripts. Uses the same zero-config defaults path (localhost:3033, model aliases, no API key needed).

Usage:
  eavs ask "what is 2+2"
  cat file.txt | eavs ask "summarize this"
...


### [eavs-qcfn.4] Enrich /health endpoint with version and provider summary (P2, task)
Currently GET /health just returns 200 OK. Extend it to return useful discovery metadata so a single probe gives apps everything they need to decide if eavs is usable:

```json
{
  "status": "ok",
...


### [eavs-hhc9] Add 'not needed' option for API key in setup wizard (P2, feature)
When adding openai-compatible or ollama providers (local endpoints), the setup wizard should offer a 'not needed' option for the API key prompt. Currently users must type a value like 'not-needed' manually. The wizard should detect local provider types and default to no-key or offer it as an explicit choice.

### [eavs-a2wb] Add 'eavs setup init' for batch provider setup with model selection (P2, feature)
setup.sh currently generates eavs config.toml via bash/jq string templating, which is fragile (stdout leaking into TOML, format mismatches). Eavs should own its own config generation.

Add 'eavs setup init' command that:
1. Scans environment for known API key env vars (OPENAI_API_KEY, ANTHROPIC_API_KEY, etc.)
2. For each found key, offers to configure the provider (Y/n)
...


### [eavs-5cym.3] Support transport-aware routing for Codex requests (P2, task)
Detect whether an incoming Codex request is SSE (HTTP POST) or WebSocket (upgrade) and route accordingly.

## Details

When a client sends a request to the Codex responses endpoint:
...


### [eavs-6yxf] Add GitHub CLI (gh) credential proxy for secure agent access to GitHub (P2, feature)
Allow agents to use GitHub CLI (gh) through EAVS without exposing real GitHub tokens.

**Use case:** Sandboxed agents need GitHub access but shouldn't have direct access to credentials.

**gh CLI token storage (for reference):**
...


### [eavs-jpcw] Error message says 'eavs_' but prefix is actually 'eavs-' (P3, bug)
The proxy error message at proxy.rs:355 and proxy.rs:1816 says 'Keys must start with eavs_' but the actual KEY_PREFIX constant is 'eavs-' (hyphen). Fixed to match.

### [eavs-qcfn.5] Automatic fallback on provider errors (P3, feature)
When a request to the default model fails with a rate-limit (429) or server error (5xx), automatically retry with the configured fallback model/provider.

Behavior:
- Only applies when the original request used a named alias (default, fast, reasoning) -- explicit model requests are not retried on a different model
- Add X-Eavs-Fallback: true header when fallback is used
...


### [eavs-vsey] Generic API key management for non-LLM providers (Sora, ElevenLabs, etc.) (P3, feature)
Eavs currently only supports LLM chat completion providers. Users need to store and proxy API keys for non-LLM services like:

- OpenAI Sora (video generation)
- ElevenLabs (TTS/audio)
- Replicate (multi-modal)
...


## Closed

- [eavs-qcfn] Auto-discovery: model defaults and zero-config endpoint (closed 2026-03-11)
- [eavs-w6jn] Change default port from 3000 to 3033 (closed 2026-03-11)
- [eavs-k8e7] Create Pi skill: eavs-auto-discovery for adding zero-config LLM access to any app (closed 2026-03-11)
- [eavs-bjsw] Enhanced mock provider: realistic streaming, tool calls, error simulation, configurable scenarios (closed 2026-02-25)
- [eavs-bjsw.17] Register mock models in /providers/detail so they appear in models.json generation (mock/simple-text, mock/tool-call, etc.) (closed 2026-02-25)
- [eavs-bjsw.16] Full chunk audit logging: log every SSE chunk sent by mock provider with timestamps for debugging (closed 2026-02-25)
- [eavs-bjsw.15] Configurable delay via X-Mock-Delay-Ms header (default 30ms per chunk) (closed 2026-02-25)
- [eavs-bjsw.14] Scenario selection via X-Mock-Scenario request header and per-model config in eavs.toml (closed 2026-02-25)
- [eavs-bjsw.13] Mock scenario: malformed_sse -- return broken SSE formatting (missing data: prefix, double newlines in wrong places) (closed 2026-02-25)
- [eavs-bjsw.12] Mock scenario: long_text -- stream 500+ tokens to test backpressure and buffer handling (closed 2026-02-25)
- [eavs-bjsw.11] Mock scenario: thinking -- stream thinking/reasoning content blocks before main response (Anthropic extended thinking format) (closed 2026-02-25)
- [eavs-bjsw.10] Mock scenario: connection_reset -- drop TCP connection mid-stream after N chunks (closed 2026-02-25)
- [eavs-bjsw.9] Mock scenario: timeout -- accept request, hold connection open without sending data (test client-side timeout) (closed 2026-02-25)
- [eavs-bjsw.8] Mock scenario: server_error -- return 500/503 with OpenAI-format error JSON (closed 2026-02-25)
- [eavs-bjsw.7] Mock scenario: rate_limit -- return 429 with Retry-After header and proper error body (closed 2026-02-25)
- [eavs-bjsw.6] Mock scenario: error_mid_stream -- stream 3 normal chunks then emit an SSE error event (closed 2026-02-25)
- [eavs-bjsw.5] Mock scenario: tool_call_then_text -- after tool result in follow-up request, respond with text completion (closed 2026-02-25)
- [eavs-bjsw.4] Mock scenario: multi_tool -- emit two sequential tool_calls in one response (closed 2026-02-25)
- [eavs-bjsw.3] Mock scenario: tool_call -- emit properly formatted tool_call chunks (function name + streamed JSON args + finish_reason=tool_calls) (closed 2026-02-25)
- [eavs-bjsw.2] Mock scenario: simple_text -- stream realistic word-by-word text response with configurable delay (closed 2026-02-25)
- [eavs-bjsw.1] Extract mock provider into src/mock_provider.rs from inline proxy.rs handler (closed 2026-02-25)
- [eavs-6mtk] Export adapters (codex, aider, goose) bypass eavs proxy (closed 2026-02-19)
- [eavs-12wz] Confusing Azure AI Foundry setup: 3 separate menu entries instead of 1 (closed 2026-02-19)
- [eavs-83mb] Add allow/deny lists for network proxy mode (closed 2026-02-17)
- [eavs-08jz] Track upstream rate limit quotas from response headers (closed 2026-02-17)
- [eavs-yrhd] Add proxy allow/deny lists (closed 2026-02-17)
- [eavs-rca1] Add domain allow/deny lists to EAVS proxy (closed 2026-02-17)
- [eavs-5cym.2] Add WebSocket proxy handler for Codex responses endpoint (closed 2026-02-17)
- [eavs-5cym.1] Recognize gpt-5.3-codex-spark model in provider detection (closed 2026-02-17)
- [eavs-y1sb] Integrate models.dev as external model catalog (closed 2026-02-17)
- [eavs-5cym] GPT-5.3-Codex-Spark support and WebSocket transport for Codex Responses (closed 2026-02-17)
- [eavs-7dkk] Multi-account support for same provider (OAuth subscription pooling) (closed 2026-02-17)
- [eavs-te85] Fix test-all summary counting 'Continue anyway' as passed (closed 2026-02-16)
- [eavs-7qma] Improve GitHub Copilot OAuth: add Copilot token exchange, dynamic base URL, and header injection (closed 2026-02-10)
- [eavs-86bw] Add secure credential storage using system keychain (macOS Keychain, libsecret, Windows Credential Manager) (closed 2026-02-10)
- [eavs-pyz3] Remove foundry provider type - Azure AI Foundry is a hosting platform, not a distinct API (closed 2026-02-04)
