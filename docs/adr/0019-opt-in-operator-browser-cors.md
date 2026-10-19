# Opt-in operator browser CORS

Status: Implemented, verification tracked in `eavs-hpy5` (`flash-eavs-cors`).

## Decision

The operator HTTP listener accepts an optional `[server].cors_allowed_origins`
list of exact serialized HTTP(S) origins. Empty is the default and grants no
cross-origin browser access. Invalid/noncanonical origins fail listener startup;
wildcards, opaque origins, paths, userinfo, queries and fragments are rejected.

Install tower-http CORS outside listener authentication. Browser OPTIONS requests
carry no capability and must terminate before authentication or proxy/provider
handlers. Actual requests still traverse the unchanged listener authentication,
virtual-key admission and route authorization. Preflight success is transport
permission only, not model discovery or authorization; nonexistent routes may
also have successful preflight. Disallowed origins receive no Allow-Origin;
CORS is browser enforcement, not a server-side origin firewall.

Permit GET/POST and a bounded set of inference headers: Authorization,
Content-Type, Accept, X-Eavs-Token, X-Provider, Anthropic-Version and Anthropic-Beta.
Expose Retry-After and X-Request-Id when present. No cookies, Allow-Credentials,
private-network permission or wildcard headers/origins. Native clients remain
unaffected. The layer adds headers to auth/handler errors and SSE responses
without collecting the response body. The separate device ingress is unchanged.

## Boundaries and alternatives

Broad permissive CORS would let arbitrary sites read an authenticated operator
response if they obtained a capability. An explicit origin allowlist is a
narrower opt-in transport policy; it does not turn the broad listener capability
into a safe browser credential. Do not distribute the operator capability to
Flash/browser clients or inject it via a reverse proxy. Ordinary virtual keys
alone still fail listener authentication. ADR0018 device ingress accepts only
enrolled device keys and Pi messages, not Flash's OpenAI/Anthropic contract.
A new inference-only ingress design is required before Flash can safely connect;
this CORS plumbing neither supplies it nor justifies exposing operator/admin
routes externally.
Admin mutation methods beyond POST are deliberately not enabled by this
inference-oriented policy. Cross-origin WebSocket authorization is not provided
by CORS, and credential-in-URL workarounds are not added.

HTTPS frontends need an HTTPS gateway with a trusted certificate (e.g. a future
external reverse proxy); CORS does not solve mixed content. TLS deployment,
proxy-level authentication and running configuration changes are outside this
change. A proxy enforcing auth on OPTIONS must explicitly support preflight too.
