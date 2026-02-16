# Issues

## Open

### [eavs-5cym.1] Recognize gpt-5.3-codex-spark model in provider detection (P1, task)
Update ProviderType detection in provider.rs to recognize gpt-5.3-codex-spark and route it to ProviderType::OpenAICodex.

## Changes needed

### src/provider.rs - detect_provider_from_model()
...


### [eavs-5cym] GPT-5.3-Codex-Spark support and WebSocket transport for Codex Responses (P1, epic)
Add support for the new GPT-5.3-Codex-Spark model and the WebSocket transport for the openai-codex-responses API, aligning eavs with pi-mono v0.52.12 changes.

## Context

OpenAI released GPT-5.3-Codex-Spark on Feb 12, 2026 -- a smaller GPT-5.3 variant optimized for real-time coding at 1000+ tok/s on Cerebras hardware. Text-only, 128k context, research preview for ChatGPT Pro users via Codex app/CLI/IDE (not public API at launch).
...


### [eavs-83mb] Add allow/deny lists for network proxy mode (P1, task)
Add allowlist/denylist support for proxy-based network isolation (domain/IP/CIDR). Ensure rules are enforced for outbound requests, with clear precedence and tests.

### [eavs-5cym.3] Support transport-aware routing for Codex requests (P2, task)
Detect whether an incoming Codex request is SSE (HTTP POST) or WebSocket (upgrade) and route accordingly.

## Details

When a client sends a request to the Codex responses endpoint:
...


### [eavs-5cym.2] Add WebSocket proxy handler for Codex responses endpoint (P2, feature)
Add WebSocket upgrade handling for the Codex responses endpoint so eavs can proxy WebSocket connections (key management, rate limiting, logging) when clients use the new WebSocket transport.

## Context

pi-mono v0.52.12 added WebSocket transport for openai-codex-responses. When a client uses transport=websocket, it opens a WebSocket to wss://chatgpt.com/backend-api/codex/responses instead of an HTTP POST. eavs currently only proxies WebSocket for /v1/realtime (OpenAI Realtime API).
...


### [eavs-6yxf] Add GitHub CLI (gh) credential proxy for secure agent access to GitHub (P2, feature)
Allow agents to use GitHub CLI (gh) through EAVS without exposing real GitHub tokens.

**Use case:** Sandboxed agents need GitHub access but shouldn't have direct access to credentials.

**gh CLI token storage (for reference):**
...


### [eavs-yrhd] Add proxy allow/deny lists (P2, feature)
Support allowlist/denylist controls for EAVS proxy routing (configurable hosts/domains/IPs) so Octo sandbox proxy can enforce outbound network policies.

### [eavs-rca1] Add domain allow/deny lists to EAVS proxy (P2, feature)
Extend EAVS config and proxy to support domain allowlist/denylist for network proxy mode. Include config schema updates, enforcement logic, and tests.

## Closed

- [eavs-te85] Fix test-all summary counting 'Continue anyway' as passed (closed 2026-02-16)
- [eavs-7qma] Improve GitHub Copilot OAuth: add Copilot token exchange, dynamic base URL, and header injection (closed 2026-02-10)
- [eavs-86bw] Add secure credential storage using system keychain (macOS Keychain, libsecret, Windows Credential Manager) (closed 2026-02-10)
- [eavs-pyz3] Remove foundry provider type - Azure AI Foundry is a hosting platform, not a distinct API (closed 2026-02-04)
