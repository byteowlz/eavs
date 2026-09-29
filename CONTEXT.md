# Eavs inference access

Eavs mediates inference access between callers and upstream providers. This glossary includes the agreed provisioning vocabulary; definitions do not imply that every capability is implemented.

## Language

**Provider**: A configured upstream service and its protocol identity. A provider is not necessarily a single account or model.

**Model**: An upstream inference capability identified within a provider. Identical model names on different providers are not automatically interchangeable.

**Route**: A model access path with a protocol, billing provenance and credential owner.
_Avoid_: Using provider, account and route interchangeably.

**Principal**: The authenticated identity to which access and ownership are attributed.

**Virtual key**: An eavs-issued credential granting a principal bounded gateway access, distinct from an upstream credential.

**Run grant**: A revocable authorization for one agent run, bounded by its parent authority and a specific loadout revision.

**Loadout**: A named, versioned selection of permitted routes, aliases, constraints and explicit fallback choices.
_Avoid_: Treating a model-picker list as an authorization boundary.

**Harness**: The agent runtime consuming inference, such as Pi, OpenCode or fx. A harness is distinct from the orchestrator that launches it.

**Account**: An upstream identity with its own credentials and entitlements.

**Subscription entitlement**: Upstream access backed by a subscription rather than a per-request API billing arrangement. It is distinct from how the caller authenticates to eavs.

**Account pool**: An explicitly eligible set of accounts behind a route. Pool selection does not authorize changing the requested model or billing arrangement.

**Fallback chain**: An ordered, explicit set of alternative routes allowed when the preferred route is unavailable.

**Peer**: Another authenticated eavs instance able to serve selected routes.

**Credential owner**: The instance responsible for holding and refreshing an upstream credential.

**Reservation**: Budget held for an outstanding operation, not yet established as actual spend.

**Settlement**: Reconciliation of a reservation against verified billable usage or an explicitly configured billing contract.

**Adoption**: Importing harness provider/model declarations into eavs; it does not inherently modify the source harness.

**Connection**: Configuring a harness to access an approved selection of eavs routes.

**Device profile**: Device-specific configuration layered over shared declarations; it is not proof of device identity.
