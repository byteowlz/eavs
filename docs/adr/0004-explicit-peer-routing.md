# Explicit peer routing without credential replication

Status: Proposed implementation of the agreed mesh direction (`eavs-4m7n.5`).

Begin mesh operation with explicitly enrolled peers over an existing private network and direct forwarding to the serving node. Preserve the originating principal's restrictions, subscription provenance and session affinity. Avoid building custom network traversal or a replicated secrets store as prerequisites.

## Consequences

Peers authenticate independently of Git configuration. Forwarded scope claims need authenticated binding, not trusted caller-controlled headers. Hop limits and route identity prevent loops. Discovery and health are live observations, not versioned configuration facts.

Distributed limits require a designated accounting authority; independent local counters do not establish a global budget. The enrollment, attenuation and accounting wire protocols must be reviewed and tested before this ADR is accepted.

## Alternatives considered

A mandatory central server reduces routing complexity but adds an unnecessary dependency for local inference. Fully automatic transitive routing and replicated OAuth tokens broaden the trust boundary before direct forwarding is proven.

See [credential ownership](0002-subscription-credential-ownership.md) and [scopes](0001-gateway-enforced-loadout-scopes.md).
