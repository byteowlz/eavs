# Scoped serving catalog

`GET /v1/eavs/catalog` returns schema version 1, a read-only projection of the running gateway's chat candidates. Source implementation under verification; not a released/installed feature. Future file exports and Pi provider integration will use this contract; neither has been migrated yet.

## Authentication

Send a currently valid eavs virtual key in `Authorization: Bearer …`. On a listener configured with a capability token, also send `X-Eavs-Token: …`; the listener token is a separate authority, not a virtual key. Missing/invalid/disabled/expired/not-yet-active virtual keys receive 401 `catalog_unauthorized`. A master key cannot discover as an unrestricted principal. Disabled/uninitialized key storage returns 503 `catalog_unavailable`.

Do not put literal credentials in shell history. Resolve the two credentials locally through environment/secret storage, and never send upstream keys to this endpoint.

## Version 1 shape

```json
{
  "schema_version": 1,
  "scope": "key-filtered-chat-candidates",
  "consistency": "file-snapshot-with-live-stores",
  "file_routing_generation": "opaque-file-only-id",
  "providers": [{
    "id": "local",
    "api": "openai-completions",
    "base_path": "/local/v1",
    "request_path": "/local/v1/chat/completions",
    "operation": "chat",
    "metadata_source": "operator_declaration",
    "compat": null,
    "models": [{
      "id": "declared-chat-model",
      "name": "Public display label",
      "reasoning": false,
      "input": ["text"],
      "context_window": 128000,
      "max_tokens": 16384,
      "compat": {},
      "pricing": null
    }]
  }]
}
```

IDs are physical models within a concrete provider. A client must bind relative paths to its explicitly configured gateway origin; never infer an upstream destination or use a response-provided absolute URL. No upstream URLs, credential references, header names/values, OAuth bindings, key identity or arbitrary config fields are returned. IDs and labels are intentionally public declaration fields; do not store secrets in those fields.

`api` is one of `openai-completions`, `openai-responses`, `openai-codex-responses`, `anthropic-messages`. Codex here describes HTTP Responses, not WebSocket support. Provider compatibility uses the existing snake_case settings; model compatibility uses an allowlisted subset of Pi-style camelCase flags. Unknown flags are not passed through. Breaking changes require a schema-version change.

`metadata_source` is `operator_declaration` (file/config/DB shortlist) or `catalog_tool_text_candidate` (already-loaded catalog entry with text output and tool support). These are metadata provenance, not upstream capability verification. Shortlists are presentation declarations, not a substitute for gateway permissions.

`pricing` is null for missing/default-zero/invalid rates. Otherwise it is `{ "status": "unverified_estimate", "unit": "USD_per_million_tokens", "rates": { "input": 1, "output": 2, "cache_read": 0, "cache_write": 0 } }`. Estimates can be incomplete, stale or incompatible with endpoint/tier billing. Do not treat them as metering, caps, confirmed free inference or a price guarantee.

## Selection and safety

- File declarations win case-insensitively over enabled DB declarations. Invalid/unroutable rows are omitted. DB shortlists override stored config shortlists; an empty DB shortlist falls back to config, then catalog.
- Existing key allow/block patterns apply to physical models; blocked wins. Empty scopes produce an empty provider list, not a broader fallback.
- Canonical path policy applies. Denied or model-rewritten candidates are omitted even if the destination model is also permitted: no stale metadata is transferred across aliases.
- Google/Vertex URL-model routes, decisions/classifiers, images, embeddings, rerank, speech and jobs are not advertised in this first slice. Add them only with explicit operation/transport/capability and accounting contracts.
- No auth command execution, referenced-file reads, OAuth refresh, network catalog load, provider probe or inference occurs. Discovery does not increment inference quotas/usage. Budget exhaustion does not revoke metadata scope.
- Responses (including endpoint errors) use `Cache-Control: private, no-store` and `Vary: Authorization, X-Eavs-Token`. Do not share response caches between credentials. Listener-middleware errors remain controlled by that middleware.
- More than 256 inventory names, 4096 collected models, or 4 MiB output returns 503 `catalog_limit_exceeded`; no partial catalog is returned. Limits include candidates later excluded by scope, so an oversized operator inventory can make discovery unavailable.

## Honest consistency

File routing is pinned for each operation. The opaque generation covers only file declarations, not DB models/providers, key scope, pricing or catalog refresh. DB data can change independently without that generation changing. Collection rechecks key validity/scope after database waits; this is not a globally atomic or durable authorization snapshot. Every inference request still passes current gateway checks, budgets and policy. Catalog presence is not health, credential availability or permission to bypass request-time checks.

Older discovery endpoints retain listener-wide behavior. Using this endpoint does not make older metadata routes tenant-private. No default selection, automatic paid fallback, writes, config sync, installs or harness file changes occur.

## Verification

`just test-serving-catalog` runs offline fixtures with temporary SQLite stores and an upstream tripwire. Coverage includes listener/key separation, master-key rejection, expiry/future activation/revocation, allow/block scopes, policy deny/rewrite/set-field, live DB updates/file ownership, file reload, redaction, compatibility allowlisting, limits and zero inference quota/usage.

Decision: [ADR0010](adr/0010-scoped-serving-catalog.md). Tracker: `eavs-peg6`.
