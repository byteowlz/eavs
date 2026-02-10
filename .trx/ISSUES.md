# Issues

## Open

### [eavs-83mb] Add allow/deny lists for network proxy mode (P1, task)
Add allowlist/denylist support for proxy-based network isolation (domain/IP/CIDR). Ensure rules are enforced for outbound requests, with clear precedence and tests.

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

- [eavs-7qma] Improve GitHub Copilot OAuth: add Copilot token exchange, dynamic base URL, and header injection (closed 2026-02-10)
- [eavs-86bw] Add secure credential storage using system keychain (macOS Keychain, libsecret, Windows Credential Manager) (closed 2026-02-10)
- [eavs-pyz3] Remove foundry provider type - Azure AI Foundry is a hosting platform, not a distinct API (closed 2026-02-04)
