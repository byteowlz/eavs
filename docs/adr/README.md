# Architectural decisions

Accepted means the direction is agreed, not that implementation or enforcement is complete. Proposed protocols require review before becoming guarantees. Use sequential numbered records; supersede decisions explicitly rather than rewriting their rationale away.

- [0001: Gateway-enforced loadout scopes](0001-gateway-enforced-loadout-scopes.md) — accepted direction
- [0002: Subscription credential ownership](0002-subscription-credential-ownership.md) — accepted direction
- [0003: Harness adapters and shared operations](0003-harness-adapters-and-shared-operations.md) — accepted direction
- [0004: Explicit peer routing](0004-explicit-peer-routing.md) — proposed implementation
- [0005: Git configuration sync](0005-git-configuration-sync.md) — accepted direction
- [0006: Per-instance provider administration](0006-per-instance-provider-administration.md) — first implementation under verification

- [0007: Fleet CLI and TUI](0007-fleet-cli-and-tui.md) — first implementation under verification
- [0008: Reusable Control Room UI](0008-reusable-control-room-ui.md) — shared-design-system module; read-only/simulator adapters only
- [0009: Explicit file-provider reload](0009-explicit-file-provider-reload.md) — bounded first implementation under verification; file-only generations

- [0010: Scoped serving catalog](0010-scoped-serving-catalog.md) — bounded authenticated chat candidates; shared operation, no harness migration yet

- [0011: Additional Pi message interface](0011-additional-pi-message-interface.md) — bounded experimental native streaming and signed/cache translation; live conformance and durable settlement remain unverified

- [0012: Central Claude Code SDK session adapter](0012-central-claude-code-session-adapter.md) — proposed; pinned bridge source review, principal-bound continuity and offline proof requirements (`eavs-eb71`)

- [0013: Explicit device connections](0013-explicit-device-connections.md) — scoped metadata observation and preview/CAS inventory snapshots; no automatic route/config replication (`eavs-bz6f`)

- [0014: Native MyGo desktop device client](0014-native-desktop-device-client.md) — read-only native source and verified control adapter; native build proof blocked by Go/tooling prerequisites (`eavs-wnsh`)

- [0017: Atomic template-based device enrollment](0017-atomic-template-device-enrollment.md) — root-approved one-time invitations and independent scoped keys; not delegated grants (`eavs-s8ke`)

- [0021: SQLite-authoritative virtual-key authentication](0021-authoritative-key-authentication.md) — per-admission security reads; process-local usage and rate limits remain non-durable (`eavs-0dne`)

See the [domain glossary](../../CONTEXT.md), [delivery contract](../scoped-provisioning.md), and implementation epic `eavs-4m7n` in `trx`.
