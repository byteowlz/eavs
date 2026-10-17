# Explicit device connections before mesh or declaration replication

Status: Proposed, bounded client implementation under verification (`eavs-bz6f`).

## Decision

Use a standalone `eavs device` CLI as the desktop/shared operation boundary.
Enrollment records an explicit trusted origin and machine-local environment
reference to a preissued scoped virtual key, only after a successful scoped live
catalog observation. Network membership is not authorization. Keep gateway
bootstrap and scope issuance with existing administration, not an automatic
trust-on-first-discovery protocol.

Read only `/v1/eavs/catalog` with bounded verified TLS, no proxy/redirect/retry and
no inference. Report static connectivity/auth/rate/server diagnostic codes. A
successful catalog response does not prove upstream availability or admission.

Start optional sync with a sanitized public provider/model inventory, not arbitrary
provider configuration. Preview, pin candidate bytes and CAS current bytes; retain
last-good, reject malformed/dirty conflicts, never execute credential references.
Do not apply cached inventory to serving routes. Existing local declarations and
credentials remain independent of remote connectivity. Complete declaration exchange
needs a separately reviewed schema and explicit local credential bindings.

## Limits

No new mesh, grant attenuation, credential replication, upstream probe, automatic
paid fallback, Git automation, harness writer or central deployment side effect.
Single-file cooperative publication is not a multi-store recovery protocol or a
power-loss/external-writer guarantee. Transport failure does not identify a
particular VPN failure. Pi adapter `eavs-r833` remains separate.

See [client contract and executable proof](../device-connections.md),
[scopes](0001-gateway-enforced-loadout-scopes.md),
[credential ownership](0002-subscription-credential-ownership.md),
[shared operations](0003-harness-adapters-and-shared-operations.md),
[explicit peers](0004-explicit-peer-routing.md),
[declaration sync](0005-git-configuration-sync.md) and
[scoped catalog](0010-scoped-serving-catalog.md).
