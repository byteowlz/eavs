# Additional Pi message interface over gateway admission

Status: Experimental bounded native streaming implementation under verification (`eavs-zt6g`); not released.

## Decision

Expose `/{provider}/v1/pi/messages` as an additional client interface. Preserve native provider APIs and working Pi routing. Pi sends `{model, context, options}` and receives Pi assistant events; standalone Rust remains credential owner.

Reuse pinned gateway routing, principal/key admission, effective model/provider scope, policy and sanitization. Reconstruct typed Pi blocks after admission, then restore representable native signature fields before upstream transmission. Unsupported signed shapes, options and tool-error states fail closed instead of being silently flattened. Signature envelopes bind advisory route/model/API provenance, never authorization. Policy rewrites cannot grant scope or migrate signatures.

## Native seam and consequences

Chat Completions, Anthropic, Google/Vertex, Responses and Codex use native request/event adapters. A pull-driven, owned stream converts bounded SSE frames without waiting for full completion; JSON-only upstream delivery is honestly labelled buffered. Dropping downstream drops upstream. Malformed/truncated streams emit a bounded static error terminal, not successful invented usage. Native APIs remain independent of this interface.

Validate positive finite effective-model price metadata before generation. Report only verified usage counts and explicit gateway flat-rate estimates; cache counts need cache rates. Budget/TPM-capped keys are rejected before Pi inference because this seam cannot provide durable reservation/settlement. Cancellation records observed usage or a request-only record, not fabricated tokens. Provider cancellation/billing guarantees are not implied.

Signature-bearing text/thinking/tools and Anthropic redaction are restored only where the native seam can represent them. Unsupported routes and fields are rejected. Provider options have explicit bounded mappings and rejection rules; detailed support and memory limits are in the [interface contract](../pi-messages.md). Basic system sections and tool-set accumulation do not promise byte-identical transcript or prefix-cache equivalence.

## Verification and remaining boundaries

Offline native fixtures cover fragmented Unicode, thinking/tool events, cache counts, signed text/tool/redacted replay through shared admission, model/provider/key denials and policy rewrites, capped-key fail-closed, truncation/error terminals and immediate upstream release on downstream drop. Actual Pi source decoder validation uses injected fetch and explicit local fixtures, never provider probes or real inference.

No full live-provider conformance, automatic discovery/export, shared provider SDK, cross-route signature migration, authoritative tier pricing or durable streaming budget enforcement is claimed. Repository-wide strict lint debt remains a release blocker; no allowances or gate weakening justify release.

## Alternatives

Replacing native APIs would regress existing clients. A Pi-only upstream relay would not satisfy the additional-interface requirement. A sidecar owning credentials or bypassing gateway authorization would violate credential ownership. The original buffered Chat-Completions-only slice was useful proof but its lossy transformation was not full Pi conformance; it is superseded by the typed native seam, not advertised as equivalent.

See [scope](0001-gateway-enforced-loadout-scopes.md), [shared operations](0003-harness-adapters-and-shared-operations.md), and [serving catalog](0010-scoped-serving-catalog.md).
