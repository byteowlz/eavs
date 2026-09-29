# Gateway-enforced loadout scopes

Status: Accepted direction; implementation pending (`eavs-4m7n.1`).

Harness configuration is a discoverability surface, not an authorization boundary. Eavs will enforce each run's authority as the intersection of operator policy, parent authority, a pinned loadout revision and run-specific restrictions. This lets lightweight harnesses consume restricted inference without implementing a separate security system.

## Consequences

- Discovery filtering is paired with request-time enforcement, including resolved aliases, fallback routes, operations and destinations.
- Delegation only narrows permissions; parent revocation invalidates descendant access. Detailed grant storage and revocation protocols remain proposed.
- Workdir paths are labels, not proof of identity. Launchers/sandboxes protect run credentials and restrict bypass traffic.
- Unsupported budget enforcement fails closed; existing native metering requires correction before broader promises.

## Alternatives considered

Config-only model selection is easier but bypassable. Giving every agent an administrator key prevents meaningful isolation and attribution.

See [domain vocabulary](../../CONTEXT.md) and [delivery contract](../scoped-provisioning.md).
