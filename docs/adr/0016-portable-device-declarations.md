# Portable declarations and independent-device planning

Status: Bounded shared operations and `device portable` CLI implemented; synthetic verification recorded in `eavs-gam7`.

## Decision

Version 1 carries only provider origins and connectivity classes, explicit model routes,
loadouts and profiles. It is separate from scoped serving catalogs, enrollment, runtime
configuration, credentials, grants and accounting. Profiles are intent, not identity
or authority. The gateway must independently enforce effective route scope.

Machine-local bindings are a separate strict document of provider identifiers and
uppercase environment variable names. Parsing never resolves an environment value,
executes a credential command, imports a file reference or copies existing secrets.
No arbitrary provider configuration is accepted.

Publication requires preview of both current and candidate SHA-256 revisions, followed
by apply of exactly those candidate bytes against unchanged current bytes. Use a
cooperative exclusive lock, same-directory temporary files and atomic rename; retain
validated previous bytes as last-good. Malformed current files stop application.
This is not a power-loss transaction, cross-file transaction or adversarial filesystem
fence. Trusted machine-local directories are required.

Public, local and tailnet describe declared route connectivity, not authorization.
Reachability is a per-provider observation with reachable/unreachable/unknown states.
A transport failure is not evidence that Tailscale is disconnected. Planning takes
only explicit route choices approved by the selected profile, never chooses a fallback,
never sends inference and never replays a request. An explicit HEAD-only observation
operation checks 1..8 approved origins without credentials, proxies, redirects or
retries, with 2-second per-request and 17-second aggregate deadlines. Observations
pin declaration content, carry per-provider timestamps and a bounded 1..60-second TTL;
planning downgrades stale/future timestamps to unknown. Missing observations are unknown.
Unavailable tailnet declarations
remain intact while independently selected public/local routes remain usable.

## Boundaries

No inference probing, Git subprocesses, auto-pull/push, hooks, serving changes or ledger
replication are implemented here. Optional Git is a separate explicit manual workflow;
resolve merges and dirty edits before preview. Preview does not certify repository
cleanliness. Explicit HEAD observations prove transport response only, including on
non-success HTTP status; no observation may be treated as scope or admission. Forged
metadata and incorrect local clocks are not an authorization boundary. Machine-local
bindings use a separate preview/CAS publisher and never resolve environment values.
The CLI pins declaration revision for binding apply; this is not a cross-file transaction.
Actual effective inference selection/failover is deliberately not wired.

See ADR [0004](0004-explicit-peer-routing.md),
[0005](0005-git-configuration-sync.md), [0013](0013-explicit-device-connections.md)
and [operation contract](../portable-device-declarations.md).
