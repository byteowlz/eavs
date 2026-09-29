# Scoped harness provisioning

Status: delivery and verification reference; not a claim of implemented enforcement.

Architectural authority lives in the [ADR index](adr/README.md); terminology lives in [CONTEXT.md](../CONTEXT.md). The decisions below are summarized for delivery context, not a second independently editable decision log. Implementation progress and blockers live in `trx` epic `eavs-4m7n`.

## Outcome

Provision lightweight isolated agents (fx is an early target) and interactive harnesses from the same loadouts. Keep eavs standalone: Oqto and a future web UI use the same operations rather than owning separate configuration logic. Competition research belongs in the wiki, not product READMEs.

## Security boundary

Harness configuration controls discoverability and defaults. Eavs independently authorizes every request. Effective authority is the intersection of operator policy, parent credential authority, a pinned loadout revision, and per-run restrictions. Delegation can only narrow authority.

A workdir string is not an authenticated identity. A trusted launcher binds a run/workspace identity to a short-lived credential. The sandbox must protect that credential and deny direct upstream access where bypass prevention is required. Never place upstream secrets into harness configs.

Scopes cover provider/model identities, API operations, destination peers/upstreams, eligible accounts, expiry, concurrency and supported budget/rate limits. Check both requested aliases and resolved routes; every fallback candidate must pass the same policy. Unsupported enforcement fails closed. Revocation and parent revocation must invalidate descendant access. Existing jobs remain owner-bound; retrieval/cancellation must not require new spending authority, but still require current authentication and ownership.

## Domain boundaries

- Route: exact provider/model/protocol, billing provenance and credential-owning node.
- Loadout: permitted routes, aliases, explicit fallbacks and constraints; immutable revision per issued run credential.
- Account pool: eligible accounts for a route, with session affinity, quota cooldowns and explicit authorization.
- Peer: explicitly enrolled, authenticated eavs node. Git configuration is not device identity.
- Run grant: opaque, revocable credential bound to principal, parent, loadout revision and run identity.

Do not silently convert subscription traffic into paid API traffic. Subscription provenance and gateway authentication are distinct. A shared credential-owning node can serve enrolled devices without distributing rotating upstream OAuth tokens. An offline credential owner makes its route unavailable; failover needs another independently authorized route.

## Harness adapters

Inventory current Magpie integrations from pinned source; verify fx identity and config surface explicitly rather than assuming Magpie supports it. Publish a per-operation capability matrix: inspect/import, gateway connection, model selection, workdir isolation, subscription metadata, standalone sync and restoration.

Adapters transform explicit source bytes into an inventory and a proposed edit. A shared coordinator owns validation, previews, ownership tracking, backups, revision preconditions, durable recovery and application. Reject malformed input and ambiguous collisions. Never execute credential commands or follow secret-file references during inspection. Preserve unrelated config and permissions; remove only entries explicitly owned by this operation, not all eavs-prefixed entries.

Pi/OpenCode are initial format references, not the abstraction boundary. Project config may merge global models; use isolated config directories or supported launch options when required. An honest unsupported result is preferable to global mutation disguised as workdir scoping.

`adopt` imports by default. `adopt --move` imports and connects only after a reviewed plan, preserving rollback artifacts. `connect` renders a selected loadout. No live migration is implied by inspection.

## Discovery and OAuth verification

No-inference checks distinguish endpoint reachable, authentication accepted, model advertised and generation unverified. Existing provider_probe performs inference and must not be reused implicitly. Catalog failure does not prove generation failure; catalog membership does not prove generation success.

Validate Codex exchange/refresh, account isolation, virtual-key binding, upstream account headers, SSE/WS, tools and reasoning through fixtures, then an explicitly authorized live smoke test. Fix the existing OAuth test command's provider/model mismatch. Pi subscription UI needs an extension or truthful upstream metadata support, not fabricated OAuth records. Keep the optional Claude process/MCP bridge separate; revalidate current provider policy before implementation.

## Sync and mesh

Git transports versioned provider metadata, loadouts, harness declarations and device profiles. Exclude credentials, runtime databases, reservations and sessions. Missing local secret references remain unresolved and visible. Standalone Pi config sync must work without gateway adoption. Use revision checks, ordinary commits, fast-forward/reviewable merges and explicit conflict stops; never force-push or silently choose a winner.

Start mesh with configured peers over an existing private network and authenticated direct forwarding. Preserve originating authority, billing provenance and session affinity; bound hops and reject loops. Do not trust unsigned forwarded scope headers. Quota observations and liveness are ephemeral, not Git records. Distributed budget authority needs an explicit owner; do not promise global caps from independent local counters.

## Delivery gates

1. Correct metering foundations: transactional reservations, durable idempotent settlement, correct units and no generic price fallback for budget authorization. Existing additions require review before broader enablement.
2. Implement and test server-enforced narrowing grants and catalog filtering, then Codex protocol verification.
3. Adapter inventory and synthetic fixtures; safe plan/apply/rollback; fx and Pi isolated launches plus OpenCode connection.
4. Two-peer forwarding proof; Git profiles and standalone harness sync; permitted account pools.
5. Optional UI/Oqto integration over shared operations.
6. Nightly watcher and implementation agent only after a manual dry run. Pin reviewed upstream revisions; treat source as untrusted data; work in isolated worktrees without live secrets, paid inference or auto-deployment. Default output is a tested review branch. Scheduler host/time, remote and merge authority remain operator choices.

## Acceptance scenario

Launch two isolated agents with different pinned loadouts. Verify catalog filtering and direct HTTP denial of forbidden models/operations; alias/fallback/peer paths cannot broaden access. Neither agent reads the other's credentials or changes global harness config. Revoke one grant and prove its subsequent requests fail. Attribute usage to the correct run and preserve owned-job isolation. Measure startup and idle/active memory against a richer harness; do not assume resource savings.
