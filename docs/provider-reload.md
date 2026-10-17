# Explicit provider hot reload

The first source implementation is tracked in **eavs-k5gd**. It does not install
or upgrade an existing gateway. Automatic watchers, SIGHUP and a dedicated CLI
reload adapter are not included.

Enabled database-managed providers already resolve live. This operation addresses
**file-declared providers** without replacing AppState or restarting its listener.

## Operation

The existing listener capability is required on every endpoint. The existing
administrator master key is additionally required here; virtual keys are not
administrator authority. Use TLS or an authenticated private network/tunnel.
Never expose the administrator key to an embedded browser UI.

- `GET /admin/routing`: non-secret current file generation and whether a loader
  was configured at server startup.
- `POST /admin/routing/reload`: reads the server's own configured sources, validates
  and previews by default. The request cannot select another path or supply config.

Preview body:

```json
{"dry_run":true}
```

The response has `scope: "file-providers"`, `status: "planned"`, opaque
`current_generation` and `candidate_generation` IDs, `resulting_generation`, and
an allowlisted `changes` array containing provider names and added/updated/removed
labels. No credentials, header values, URLs, config contents or parser excerpts
are returned. A generation is not authorization, readiness or a price revision.

Apply the exact preview within **60 seconds**:

```json
{
  "dry_run": false,
  "expected_generation": "<current_generation from preview>",
  "expected_candidate_generation": "<candidate_generation from preview>"
}
```

Both expectations are mandatory on apply. The latest preview replaces the previous
preview. Apply rereads the sources and requires the observed candidate to equal the
staged candidate. A stale generation, superseded/expired preview or changed
candidate returns 409; there is no automatic retry. An unchanged candidate returns
`status: "unchanged"` without advancing the active generation.

All selected file layers must be TOML in this slice. Startup YAML remains supported,
but hot reload of YAML/JSON/other formats is rejected. Source precedence stays:
XDG global → selected local file → EAVS environment → explicit config file →
startup CLI host/port overrides. Reload never scaffolds a missing file or invents
an empty configuration's default provider. Removing an entry in one layer does not
remove a declaration still supplied by another layer; inspect the effective diff.

## Boundary

Supported: provider additions, ordinary HTTP/WS provider updates/removals, declared
model shortlists and implemented provider-native compatibility metadata. Disable a
file route by removing its effective declaration; there is no file `enabled` flag.
Unknown provider/compat/model-cost fields, unsupported types, ambiguous
case-insensitive names and duplicate model IDs are rejected. New/changed destinations
must be literal/default HTTP(S) URLs accepted by the unchanged network policy, with
no URL credentials, query or fragment. New reference-based destinations and dynamic
providers without a usable literal/default base URL are not activated by this slice.
Unchanged destination references are retained, not resolved during validation.

Non-provider settings are restart-only, including listener/authentication, keys,
policy, network/egress, database paths, logging, analysis, capture and transforms.
Their raw effective input is also compared so unknown nested changes cannot be
silently accepted as part of a provider reload. CLI overrides remain effective.
Mixed supported/restart-only changes reject the whole candidate.

Pricing tables/catalogs are not rebuilt. Changes to existing model cost declarations
and nonzero costs on newly declared models are rejected; removed declarations do not
erase already-loaded pricing. This is not endpoint/tier pricing qualification or
proof of durable budget enforcement. Unsupported capped native operations retain
existing fail-closed behavior.

Existing **Fal/Replicate asynchronous-job provider declarations cannot change or
be removed** through reload, including credentials/destination/protocol and metadata.
Reload does not migrate historical handles, credentials or billing ownership. These
changes require a separately qualified owner migration, not a recommendation to
restart blindly. New job providers do not enable capped job accounting.

File/store precedence is unchanged: file declarations win case-insensitively;
enabled valid stored declarations supply otherwise absent names. Adding/removing
file ownership over any colliding stored row, including disabled rows, is rejected.
Explicitly reconcile ownership first. HTTP create/update/delete/credential operations
and reload share an ownership coordinator. Independent processes/external database
writers are not supported cache-coherence participants.

## Admitted work and discovery

HTTP, WebSocket and native-job operations capture the file snapshot once. Accepted
streams keep their original route/protocol/credentials; subsequent operations use
the published generation. Removing a route does not terminate accepted work.
Authorization, OAuth-owner stores, usage/accounting, rate limiters, reservations,
conversations and sessions remain the same shared objects. Existing authorization
checks after model resolution/rewrites remain in place. Urgent socket revocation is
not added here.

HTTP inference and `/providers` return `X-Eavs-File-Routing-Generation`.
`/providers/detail` and `/defaults` include `file_routing_generation`. File additions
retain the existing implicit catalog-default provider; removing it makes that default
unavailable rather than silently selecting another provider. Declaring `default` is
an explicit selection. Missing explicit defaults return 404. These default-selection
rules concern file declarations; a unified DB/catalog default coordinator is pending.

These IDs describe **file snapshots only**, not a globally atomic database/catalog/
pricing generation. Database providers and their models remain independently live.
Invalid stored entries are omitted from discovery instead of advertised as working
OpenAI routes. No endpoint health or inference readiness is inferred.

Pi's static `models.json` and running session/default are not rewritten. Deliberately
refresh harness configuration and review its diff separately. An offline file export
is not proof that the running gateway admitted that declaration.

## Failure and persistence

Invalid/partial TOML, validation failures, failed ownership checks and stale applies
retain the active in-process snapshot. Error bodies are static classifications;
malformed JSON and TOML cannot echo secret-bearing excerpts. Preview/apply do not
call provider probes, inference, model-loading endpoints, keychains or referenced
secret files/commands. Normal owned configuration files and inherited startup
environment inputs are read; credential reference strings remain opaque.

Publication is one immutable watch-head replacement. No declaration/config/database
write or durable publication journal is performed. The owner remains responsible
for durable configuration edits and repair of rejected disk contents before restart.
An uncooperative external editor can change files after apply's read; this operation
cannot atomically fence such writers. Last-good recovery across process restart,
durable job/billing provenance, coherent DB/catalog publication, scoped admin roles
and watcher debouncing are separate work.

## Offline proof

```sh
just test-routing
just check-all-features
just test
```

Tests use isolated synthetic files, in-memory databases, injected fake upstreams and
an ephemeral loopback gateway. They cover atomic file replacement, CLI precedence,
no implicit default injection/switching, last-good rejection, stale/content-pinned
previews, concurrent CAS, secret-safe errors, source conflicts, shared runtime-object
identity, virtual-key restrictions/default/rewrite/native paths, and a delayed Unicode
SSE stream continuing on A while the same listener serves a native tool request on B.
Neither preview nor apply sends an upstream request. The full native regression suite
remains required; these tests do not prove durable accounting, every mux/SSH transport,
production TLS/admin roles or source-to-installed-binary provenance.
