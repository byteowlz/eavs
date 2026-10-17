# Additional Pi messages interface (experimental)

Pi is an additional client interface, not a replacement for native provider APIs or a credential owner. Tracked in `eavs-zt6g`; not installed or released.

## Connection and admission

Use Pi `api: "pi-messages"`, `baseUrl: "http://127.0.0.1:3033/alpha/v1/pi"` and a locally resolved scoped virtual key. Pi appends `/messages`. Unprefixed `/v1/pi/messages` requires explicit `X-Provider`. A separately configured listener capability still requires `X-Eavs-Token`. Never embed real credentials in shared configuration.

Requests are `{model, context: {messages: [...]}, options}`. The adapter uses existing pinned routing, key/provider/model authorization, effective post-policy scope, delegated-fetch sanitization and native credentials. Harness configuration and signature envelopes grant no authority. Existing native paths and Pi routing are unchanged; no automatic catalog export or client migration occurs.

## Native streaming seam

Chat Completions, Anthropic, Google/Vertex, Responses and Codex have native request/event adapters. Bedrock and unrelated inference shapes are rejected. Tested coverage is the offline native matrix, not proof of every vendor/model combination (Vertex has no dedicated fixture).

Pull-driven SSE yields Pi `start`, text/thinking/tool start/delta/end and one `done` or static `error`. Unicode can cross upstream byte boundaries. `done` requires valid terminal reason and usage; incomplete, private upstream error or malformed streams never fabricate successful zero usage. `X-Eavs-Pi-Delivery` is `streaming`, or `buffered-upstream` for JSON upstream responses. Dropping the downstream body drops the owned upstream stream; this does not guarantee that a provider stops billing.

Signed text, thinking, tool calls and Anthropic redacted thinking use opaque route/model/API provenance envelopes. Replay restores native fields after shared policy and before sending upstream. Changed route/model/API, unsupported signature placement and unsupported raw shapes fail rather than flattening signatures. Envelopes are advisory provenance, **not cryptographic authorization**. Signed Responses reasoning preserves its native item; callers must not rewrite signed contents. Shared empty-text normalization applies only to the surrogate, not the reconstructed canonical blocks.

Text tools/results and inline PNG/JPEG/WebP/GIF images are accepted. Error-marked tool results are preserved for Anthropic and rejected on other routes, whose current serializers cannot preserve error semantics. Tool definitions/sections are accumulated from system messages; this is not byte-identical transcript or prefix-cache equivalence. Constrained sampling is rejected.

## Options and limits

- `maxTokens` must be a positive u32 (at least 16 on Responses/Codex; rejected rather than clamped), temperature finite in `[0,2]` and preserved without f32 rounding. Named tool choice must have exactly `{type:"function",function:{name}}`; Responses receives its flattened native form. Google tool choice is currently rejected.
- Reasoning levels map explicitly to native effort or native budgets: minimal/low=1024, medium=4096, high/xhigh/max=8192 for Anthropic/Google; off disables the requested reasoning mode. These are adapter conventions, not model capability promises. Anthropic thinking requires a larger output limit and rejects simultaneous temperature rather than silently removing it.
- Session ID is advisory upstream cache key on OpenAI-shaped routes; Anthropic/Google reject it. Anthropic supports short/default cache and none (removes explicit cache controls), but not long. Google explicit retention is rejected. OpenAI-shaped long maps to `24h`; none omits session cache key, not a promise to disable provider caching.
- Budget- or TPM-capped virtual keys fail closed before inference (`pi_capped_accounting_unavailable`). Observed usage is not durable reservation/settlement. On cancellation/error, available validated counts are recorded; absent counts record only the request, never invented token usage.
- Positive finite flat input/output price metadata is required before generation, including the effective model. Nonzero cache counts require explicit matching cache rates. `X-Eavs-Pi-Cost: estimated` means gateway flat-rate estimate, not authoritative billing, tier-aware pricing or durable enforcement.
- Request body: configured limit, or 10 MiB when unlimited. Upstream JSON: 16 MiB. SSE frame and upstream SSE chunk: 1 MiB; stream block keys: 1 KiB; output text/tool aggregate: 16 MiB, per block 4 MiB, at most 1024 blocks. Signatures: 1 MiB each, charged against the aggregate bound. Oversized/malformed input fails closed. Streaming projection errors can follow incurred inference; never automatically retry them.

Residual limits: provider capability discovery, export/shared SDK integration, tier-aware authoritative pricing, durable capped streaming accounting, cross-route signature migration and verified live-provider coverage remain out of scope. No real credentials, inference probes, paid generation, installation or restart are used by verification.

## Offline proof

```sh
just check
EAVS_PI_NATIVE_FIXTURES=/tmp/eavs-pi-native-fixtures just test-pi-messages
bun scripts/test_pi_messages_sdk.ts ../external-repos/pi-mono /tmp/eavs-pi-native-fixtures
just clippy
```

Fixtures use temporary stores and injected upstream only. Coverage includes native Unicode/thinking/tools/cache events, signed text/tool/redacted replay through shared admission, invalid key/provider/model and policy rewrite denials, capped-key rejection, incremental delivery, downstream drop, truncation/private-error terminals and unsupported options/signatures/tool errors.

Actual Pi source: `packages/ai/src/api/pi-messages.ts`, checkout commit `87f9a36419e8164bba77397d15c54ff39fac5e8a`. The SDK script injects fetch and accepts a single SSE file or the native fixture directory, without package installation/network. Reference artifact: <https://radius.earendil.com/artifact/01m4xtpkqefybt0pvrc5ry16xz> v2; source remains wire authority. Repository-wide strict gates may fail on unrelated dirty-tree lint debt; that is not waived by scoped fixture success.
