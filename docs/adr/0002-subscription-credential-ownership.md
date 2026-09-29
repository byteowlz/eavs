# Keep subscription credentials with their serving gateway

Status: Accepted direction; protocol verification pending (`eavs-4m7n.2`).

Keep upstream OAuth credentials and refresh responsibility on their owning eavs instance. Harnesses authenticate to eavs using scoped gateway credentials; subscription entitlement remains separate metadata from gateway authentication. This enables one upstream login to serve enrolled devices without copying rotating refresh tokens.

## Consequences

Subscription and metered API routes remain distinct. Paid fallback needs explicit permission. Expose truthful billing provenance and preserve native protocol capabilities rather than merely adding a subscription label. A Pi extension and possible upstream metadata change are integration proposals, not implemented behavior; do not fabricate OAuth state to obtain a badge.

If the credential owner is unavailable, its route is unavailable. Another account or independently authorized instance is a separate route, not automatic credential failover. Provider authorization and terms still constrain subscription sharing and pooling.

## Alternatives considered

Copying refresh tokens through Git or between all peers introduces disclosure and refresh races. Requiring every harness to log in upstream defeats centralized credential ownership. A permanent central gateway is supported, but is not mandatory for all local inference.

See [mesh boundary](0004-explicit-peer-routing.md).
