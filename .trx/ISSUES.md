# Issues

## Open

### [eavs-83mb] Add allow/deny lists for network proxy mode (P1, task)
Add allowlist/denylist support for proxy-based network isolation (domain/IP/CIDR). Ensure rules are enforced for outbound requests, with clear precedence and tests.

### [eavs-6yxf] Add GitHub CLI (gh) credential proxy for secure agent access to GitHub (P2, feature)
Allow agents to use GitHub CLI (gh) through EAVS without exposing real GitHub tokens.

**gh CLI token storage:**
- macOS: System Keychain (service: gh:github.com, base64 encoded)
- Linux: GNOME Keyring / libsecret, or ~/.config/gh/hosts.yml plaintext fallback
...


### [eavs-7qma] Improve GitHub Copilot OAuth: add Copilot token exchange, dynamic base URL, and header injection (P2, feature)
Borrow improvements from pi-mono's GitHub Copilot implementation:

1. **Copilot token exchange** - After GitHub OAuth, exchange token via /copilot_internal/v2/token to get Copilot-specific token
2. **Dynamic base URL** - Parse proxy-ep from Copilot token to determine correct API endpoint (api.individual.githubcopilot.com vs enterprise)
3. **Header injection** - Add Copilot-specific headers based on request context:
...


### [eavs-yrhd] Add proxy allow/deny lists (P2, feature)
Support allowlist/denylist controls for EAVS proxy routing (configurable hosts/domains/IPs) so Octo sandbox proxy can enforce outbound network policies.

### [eavs-rca1] Add domain allow/deny lists to EAVS proxy (P2, feature)
Extend EAVS config and proxy to support domain allowlist/denylist for network proxy mode. Include config schema updates, enforcement logic, and tests.

## Closed

- [eavs-pyz3] Remove foundry provider type - Azure AI Foundry is a hosting platform, not a distinct API (closed 2026-02-04)
