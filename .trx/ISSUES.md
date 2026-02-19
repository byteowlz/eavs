# Issues

## Open

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
