# Scoped running-instance serving catalog

Status: First bounded implementation under verification (`eavs-peg6`).

## Decision

Expose `GET /v1/eavs/catalog` through a shared read-only operation, independent of any harness or controller. Require a currently valid virtual key and the listener's existing authentication middleware. Never substitute listener or master credentials for virtual-key scope. Harness exports and Pi provider integrations will consume this contract in later slices, not repeat local-file discovery logic.

Pin file routing once, collect live enabled stored-provider declarations using the existing file-first resolver, then reauthenticate after asynchronous DB reads and project synchronously through current provider/model permissions. Apply existing policy to a canonical chat request model and path. Omit denied, removed/non-string, or rewritten model candidates rather than copying old capability/pricing metadata to a different physical model. Request-time authorization remains authoritative.

Use a versioned allowlisted DTO with relative gateway paths. Never resolve upstream secrets, commands, files, OAuth credentials or refresh flows. No provider probes or inference. No inference rate/token/budget charges. Metadata may remain readable for an exhausted key; a catalog entry is not admission.

## Bounded first slice

Chat HTTP candidates only, using verified existing protocol mappings; Google/Vertex URL-model paths and non-chat operations are excluded pending dedicated contracts. Explicit shortlists are operator chat declarations, not verified upstream capability. Without a shortlist, only already-loaded catalog entries with text output and tool support qualify; no catalog fetch occurs. Aliases/default selection and WebSocket transport promises are absent.

Prices are unverified token-rate estimates, never authoritative billing. All-zero/missing or invalid rates produce null rather than a claim of free inference. Compatibility fields are allowlisted, not arbitrary config passthrough. Public IDs/display labels are intentional operator metadata, not credential fields.

Responses are private/no-store, with no secret-derived hashes or ETags. Limits: 256 inventory names, 4096 collected model entries, 4 MiB serialized DTO. Exceeding a limit rejects the whole operation with a static code rather than silently truncating it.

## Consistency and limits

The file generation describes file declarations only. Provider/model DB reads, catalog data, keys and pricing are not one transactional revision. Revocation observed after DB waits rejects discovery; it is not a globally linearizable revocation protocol. Concurrent store changes can yield mixed metadata, so clients must refresh and every inference request must reauthorize. No health, availability, entitlement, budget or completion claims are made.

Legacy `/providers`, `/providers/detail` and `/defaults` retain their existing listener-wide behavior for compatibility. This endpoint does not establish tenant metadata isolation across those older routes. Scoped roles, operation grants, durable budgets and a coherent serving revision remain separate work.

## Alternatives

Wrapping `/providers/detail` would retain arbitrary integration metadata and bypass virtual-key scope. Reusing inference validation would spend quota on model-picker refresh. Generating separate per-harness inventories would create inconsistent routing/auth filtering. A central replicated config/credential store is neither required nor introduced.

See [gateway scope](0001-gateway-enforced-loadout-scopes.md), [shared operations](0003-harness-adapters-and-shared-operations.md), [file reload](0009-explicit-file-provider-reload.md), and [endpoint contract](../serving-catalog.md).
