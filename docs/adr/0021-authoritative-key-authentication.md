# SQLite-authoritative virtual-key authentication

Status: Implemented for `eavs-0dne` (key-cache-coherence); offline verification only.

## Decision

Every virtual-key authentication reads the committed SQLite row through async
`KeyStore::lookup_for_auth`. This includes KeyValidator (and skip-budget job
lifecycle admission), native inference ingress, serving catalog's initial and
post-inventory checks, device ingress and Pi preflight. A cache hit is never
sufficient authority. Missing/deleted rows, database failures and malformed stored
security metadata fail closed; errors never fall back to cache. Existing validity,
physical provider/model scope, capped-operation refusals and rate-limiter checks
remain at their original seams. External creation and changes to validity, scopes,
metadata and OAuth bindings take effect on subsequent reads without restart.

A successful read publishes the metadata to the local cache with a DashMap entry
operation, preserving the existing usage object under the same entry lock used by
usage increments. It does not clear counters, pending writes or rate-limiter
state. Missing/malformed rows may leave old usage snapshots in memory, but these
snapshots cannot authenticate. Synchronous cache getters remain local reporting,
identifier-resolution and test conveniences, not authentication APIs. The explicit
full `refresh_cache` is a startup/reporting loader, not an auth reload mechanism.

No DashMap guard may survive a SQL await. Usage flushes clone usage first and
UPDATE only the usage column of existing rows: they cannot recreate deleted rows
or restore older permissions, validity, disabled status or bindings. Existing
partial administrative owner/OAuth setters likewise UPDATE only their named
columns, rather than writing back cached whole-key metadata.

## Consistency and limits

The SQLite read is an admission snapshot, not globally linearizable revocation:
a request whose read preceded a concurrent revoke may continue. Independent
concurrent admissions may observe different revisions. No new cross-row revision,
parent grant protocol, cancellation or upstream request revocation is introduced.
The catalog continues rechecking after asynchronous inventory reads.

Local usage takes precedence over the persisted usage snapshot on a cache hit so
an auth read cannot discard unflushed increments. Aggregates are still whole-value
write-behind snapshots, not transactional deltas: concurrent gateway processes or
out-of-order flushes may overwrite aggregate usage, and a crash can lose pending
usage. Rate limits remain process-local. Sharing a database does **not** establish
durable or shared budgets/quotas; existing unsupported capped operations still
fail closed. Usage-history records are separate from these aggregate snapshots.
Operators should use one serving process per key database for local accounting;
direct CLI provisioning is now visible to authentication, not an accounting
coordination mechanism. Older gateway binaries remain cache-only and must be
upgraded/restarted to obtain this behavior. No timer or miss-only fallback is used.

## Verification

Two independent stores sharing temporary SQLite exercise external creation at
real validator, native HTTP and catalog seams. Tests also cover external disable,
delete, scope narrowing, malformed rows/database failure, and concurrent auth,
usage increments, partial admin updates and usage flushes on a current-thread
runtime. Native inference uses only a synthetic upstream; catalog has a tripwire
against generation. Existing routing/CORS/native-shape work remains independent.

This supersedes the authentication cache-coherence limitations described in
ADRs [0017](0017-atomic-template-device-enrollment.md),
[0018](0018-opt-in-device-inference-ingress.md) and
[0020](0020-scoped-native-inference-ingress.md), but not their accounting,
invitation, listener-isolation or deployment limitations.
