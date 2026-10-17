# Explicit file-provider routing publication

Status: First implementation under verification (`eavs-k5gd`); not the complete
coherent-routing/durable-accounting protocol proposed on the messageboard.

## Context

The hot-reload proposal in message `a27239ef-2db7-43f5-87a4-f0db8cc439d2`
was read from a clean upstream checkout at `1175587`. The existing board checkout
was divergent and left untouched. Its source report predates the current working
implementation: enabled admin-store providers now route live through
`AppState::resolve_provider`. File providers still used an immutable startup Arc.
No installed-binary provenance or live deployment change was established.

## Decision

Keep runtime services and authorization stores intact. Add one shared explicit
preview/apply coordinator which atomically replaces an immutable **file-provider**
snapshot. Use existing Tokio watch/Mutex primitives, not a new ArcSwap dependency.
Pin the file snapshot once at HTTP/WS/job admission; preserve native provider
configurations and all existing post-resolution/rewritten-model authorization checks.

Use the server's startup loader precedence and CLI overrides, without bootstrap
writes, secret-reference resolution or inference probes. Initially support TOML
sources only. Validate the supported provider schema and destination policy, retain
last-good state on rejection, and reject non-provider/restart-only changes. Retain
raw restart-only input as well as typed settings so ignored nested changes cannot
masquerade as successfully applied configuration.

Require a content-pinned preview and current/candidate opaque generations for apply.
One bounded latest preview expires after 60 seconds; publication is serialized/CAS,
with no automatic retries. Serialize HTTP provider ownership writes through the same
coordinator. Do not silently switch a name between file and stored declarations.

Keep pricing/catalogs unchanged and reject new nonzero/changed declared prices.
Do not hot-migrate existing asynchronous-job provider declarations or historical
handles. Preserve the implicit file catalog-default provider across additions; if
removed, make it unavailable rather than select a different route automatically.

## Consequences and limits

Expose generation identifiers explicitly as **file** identifiers. Existing enabled
valid DB declarations/models remain independently live; these IDs do not establish
one globally atomic DB/catalog/pricing snapshot. A complete effective-source
publication coordinator is follow-up work, not an inferred property of this slice.

No listener, security policy, database service, limiter, ledger, reservation, OAuth
owner, session or conversation store is reconstructed. This preserves their identity,
not a claim that durable billing or urgent WebSocket revocation is complete.

The HTTP adapter requires existing broad master-key administration plus listener
capability. Scoped roles/audit, browser transport, CLI/watch/SIGHUP adapters, durable
publication recovery, external-writer fencing, historical job provenance and Pi
catalog-refresh coordination remain separate. Reading declarations and publishing
in memory is not a durable multi-store transaction (ADR-0003).

See [provider reload](../provider-reload.md) for API bodies, field boundaries,
rejection semantics and executable offline proof.

## Alternatives

Recreating AppState loses live runtime state. Mutating provider maps in place can
mix protocol/destination fields in admitted work. A picker refresh alone does not
activate inference. A watcher first introduces partial-write/event-burst semantics
before the semantic operation is proven. Publishing a hash of secret-bearing config
would enable credential guessing; use opaque staged IDs instead.
