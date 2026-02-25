# Issues

## Open

### [eavs-bjsw.17] Register mock models in /providers/detail so they appear in models.json generation (mock/simple-text, mock/tool-call, etc.) (P1, task)

### [eavs-bjsw.16] Full chunk audit logging: log every SSE chunk sent by mock provider with timestamps for debugging (P1, task)

### [eavs-bjsw.15] Configurable delay via X-Mock-Delay-Ms header (default 30ms per chunk) (P1, task)

### [eavs-bjsw.14] Scenario selection via X-Mock-Scenario request header and per-model config in eavs.toml (P1, task)

### [eavs-bjsw.13] Mock scenario: malformed_sse -- return broken SSE formatting (missing data: prefix, double newlines in wrong places) (P1, task)

### [eavs-bjsw.12] Mock scenario: long_text -- stream 500+ tokens to test backpressure and buffer handling (P1, task)

### [eavs-bjsw.11] Mock scenario: thinking -- stream thinking/reasoning content blocks before main response (Anthropic extended thinking format) (P1, task)

### [eavs-bjsw.10] Mock scenario: connection_reset -- drop TCP connection mid-stream after N chunks (P1, task)

### [eavs-bjsw.9] Mock scenario: timeout -- accept request, hold connection open without sending data (test client-side timeout) (P1, task)

### [eavs-bjsw.8] Mock scenario: server_error -- return 500/503 with OpenAI-format error JSON (P1, task)

### [eavs-bjsw.7] Mock scenario: rate_limit -- return 429 with Retry-After header and proper error body (P1, task)

### [eavs-bjsw.6] Mock scenario: error_mid_stream -- stream 3 normal chunks then emit an SSE error event (P1, task)

### [eavs-bjsw.5] Mock scenario: tool_call_then_text -- after tool result in follow-up request, respond with text completion (P1, task)

### [eavs-bjsw.4] Mock scenario: multi_tool -- emit two sequential tool_calls in one response (P1, task)

### [eavs-bjsw.3] Mock scenario: tool_call -- emit properly formatted tool_call chunks (function name + streamed JSON args + finish_reason=tool_calls) (P1, task)

### [eavs-bjsw.2] Mock scenario: simple_text -- stream realistic word-by-word text response with configurable delay (P1, task)

### [eavs-bjsw.1] Extract mock provider into src/mock_provider.rs from inline proxy.rs handler (P1, task)

### [eavs-bjsw] Enhanced mock provider: realistic streaming, tool calls, error simulation, configurable scenarios (P1, feature)

### [eavs-1sfs] Upstream 401 errors cause request to hang instead of returning error (P1, bug)

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


## Closed

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
