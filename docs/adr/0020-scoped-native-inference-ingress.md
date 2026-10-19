# Scoped native inference ingress

Status: Implementation/verification in `eavs-m09n` (`flash-eavs-inference`). No deployment approval.

## Decision

Add a third, default-disabled listener configured by `[inference].listen` (explicit
nonzero loopback IP:port). Startup fails closed if enabled without keys, initialized
same-process KeyStore/validator, valid origins or an available bind. Operator and
ADR0018 device listeners remain separate and unchanged. Never distribute/inject
operator authority to browser clients or expose operator/admin routes via Caddy.

Exhaustive routes: GET `/v1/models`; POST `/v1/chat/completions`, `/v1/messages`,
`/v1/responses`. No provider-prefixed paths, catch-all, admin, OAuth, job, credential,
configuration or WebSocket routes. Unknown routes are 404. Require a currently
valid ordinary virtual key on every actual supported request independently of
listener/operator authentication. Bearer and Anthropic X-Api-Key are accepted,
conflicting credentials rejected; X-Eavs-Token is never authority. Reject operator
and master tokens even if accidentally equal to a stored virtual key. Enrolled
device keys remain on their existing surface, not this ordinary-key contract.

Discovery projects the existing authoritative `serving_catalog::build`, never
legacy `/v1/models` proxy/provider enumeration. Public model IDs are
`provider/physical_model` strings (provider IDs exclude `/`, so collision-free).
Clients must send the full advertised ID, not a raw model name. Each model record
also carries the namespaced `eavs_api` catalog protocol tag (`openai-completions`,
`openai-responses`, or `anthropic-messages`) so clients can explicitly select a
native SDK. It grants no authority and contains no URL or credentials; native
endpoint compatibility remains enforced at request time. A request
must select an ID from the caller's current projection and a compatible native
protocol. Resolve through the same projection, replace only model with its physical
ID, force X-Provider to its catalog provider, strip alternate routing/credential
headers, then reuse the pinned native proxy pipeline. No parallel authorization,
secret lookup, accounting or transport implementation. Discovery omits aliases
and policy-rewritten candidates as ADR0010 does; this bounded ingress does not
add alias/fallback promises. Request-time key validation and effective-model scope
checks still run. Unknown/unauthorized model IDs have the same static rejection.

Budget/TPM-capped inference is explicitly refused on this new surface pending
proven durable accounting (discovery may remain available); RPM/RPD and usage
retain existing native pipeline behavior, without new durability claims.

## Browser policy and boundaries

`[inference].cors_allowed_origins` is separate from operator CORS and defaults to
empty. Reuse exact-origin, no-cookie CORS outside route auth; OPTIONS is only
transport and never invokes inference. Use a finite SDK header allowlist including
Anthropic browser/version/beta headers, known Stainless runtime/retry headers,
X-Client-Request-Id and Idempotency-Key. Do not mirror arbitrary request headers.
Forward X-Client-Request-Id and preserve Idempotency-Key retry semantics with a
stable SHA-256 namespace bound to the virtual key, preventing cross-tenant replay
collisions on shared upstream credentials. Only these semantic headers enter the
native pipeline via a typed extension; SDK telemetry is not blindly forwarded. Responses/errors are private/no-store;
SSE stays streaming. TLS is external and must route only approved exact paths to
THIS inference loopback port, never to the operator port, with a deny fallback.
No key logging, upstream/operator key injection, live config/profile/process
changes, real inference or deployment in this implementation.

Tests must prove missing/invalid/expired/revoked/operator keys denied, per-key
catalog isolation, forbidden model/protocol rejection before upstream access,
unknown/admin routes unavailable, startup prerequisites, CORS/errors/streaming,
and existing effective-scope/capped-key refusals. Same-process cache ownership is
not cross-process or globally linearizable revocation. Catalog is metadata, not
availability or budget admission. No upstream probes/fetches during discovery.
