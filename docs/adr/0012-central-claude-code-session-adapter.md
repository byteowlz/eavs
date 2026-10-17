# Central Claude Code SDK provider adapter

Status: Proposed and parked in backlog (`eavs-eb71`); source inspection only. Not implemented or approved for release.

## Evidence

Inspected MIT `elidickinson/pi-claude-bridge` v0.9.3 at commit `874fa7c2678456336f23b64e784e2057e608d83e` (read-only checkout `/tmp/eavs-pi-claude-bridge.sf9Opt`). No SDK process, login, credential/config reads, dependency install or inference was performed. Its tests were inspected, not run.

- `src/index.ts` provider path invokes the real Agent SDK with `tools: []`, exposes caller tools through MCP, and parks handlers while Pi executes them. It is distinct from the AskClaude path, which can execute Claude Code tools.
- `src/mcp-server.ts` matches handlers using Claude Code's nonstandard `_meta["claudecode/toolUseId"]`, not call order. Missing IDs fail rather than guessing. Root schema combinators are flattened for CC; this is a compatibility concession, not lossless schema support.
- `sessionStateFor` separates mirrors by Pi session ID. `syncSharedSession` uses a cursor, rewrite markers and trailing-assistant handling to choose reuse, fresh start or JSONL rebuild. This relies on in-process Pi events; it is not a sufficient remote tenant boundary or a full content-prefix comparison.
- Per-query pending call/result maps allow either arrival order. Compaction/tree changes invalidate only the affected session's parked queries. Summary calls using `cacheRetention: none` are isolated from the main mirror.
- Abort interrupts/closes the SDK query; rebuild may rotate the CC UUID to avoid racing an orphan writer. Rebuild can miss prompt caches and lose post-edit snapshots. Cwd affects session storage; it must not come from a remote client path.
- `tests/unit-cross-session-turns.mjs`, `unit-sync-shared-session.mjs`, `unit-deliver-tool-results.mjs`, `unit-unserved-tool-use.mjs` and `unit-session-cwd.mjs` provide scenario references. No claim that upstream tests passed here.

## Proposed decision

Offer a distinct, explicitly enabled SDK-backed provider adapter behind existing eavs inference admission. A supervised SDK worker on the credential-owning machine owns the CC processes and files; Rust eavs retains authentication, effective route/model authorization, limits and private errors. The worker is not a generic remote shell and must not receive arbitrary executable paths, environment overrides or working directories from callers.

Provider mode keeps tool execution in the client harness. Do not silently turn it into remote agent execution. Native CC tools, user/project hooks, ambient MCP/cloud tools, memory and unrelated machine context must be disabled or explicitly isolated and proven; do not copy the bridge's `bypassPermissions` setting as a security boundary.

Bind each server-issued opaque conversation handle to authenticated principal, enrolled client, logical session/branch, physical provider and credential owner. Session labels/cache keys are advisory lookup inputs, not authorization. Reauthorize every turn and tool-result submission, including after key/scope changes. Persist binding metadata separately from secrets, with private permissions, retention and explicit deletion rules. A server restart invalidates pending MCP handlers unless safe recovery is proven.

Track a canonical transcript revision/prefix digest, tool and instruction revisions, server generation and in-flight turn. Serialize mutation per branch; reject stale/conflicting requests rather than guessing. Changed same-length histories must not qualify as append-only. Client session/branch/rewrite events may improve behavior via an optional Pi extension, but correctness must not rely solely on receiving those events.

A normal Pi `toolUse` terminal closes one HTTP stream, not the SDK conversation: retain a bounded, scoped parked query for the next authenticated tool-result request. Match results by conversation, turn, tool-use ID and tool identity; never scan a global tool-ID map across tenants. Duplicate/conflicting/foreign/expired results cannot invoke tools or release another handler. Bound idle time, pending calls, process count and retained bytes. Actual disconnect during generation cancels that turn; normal completion is not disconnect cancellation. Do not retry inference or side-effecting tool execution automatically after an ambiguous failure.

Pi remains the source of conversation history. Compaction, fork/tree changes, changed instructions/tools, model/owner changes and restart require explicit reuse/rebuild/new-session decisions. Isolated summaries must not mutate the main conversation. Tailnet loss makes this central route unavailable, not migratable to another provider/owner/session by inference. Alternative routes require explicit authorization and may require a new conversation.

SDK estimates/quota metadata are not durable spend, and subscription entitlement is not a verified free rate. Until priced-operation/cap enforcement is defensible, capped requests fail before inference; do not invent a successful zero-cost completion. Verify current upstream supported authentication and subscription terms independently: the bridge README's billing statements are not authoritative policy evidence.

## Verification before implementation is considered complete

Use an injected fake SDK and temporary state: no credentials, child CC binary, paid generation or real user files. Prove two principals/devices with identical client session IDs and tool IDs cannot resume, mutate, cancel or observe one another. Cover parent/child and concurrent branches; out-of-order/duplicate results; same-length edited prefix; compact/tree/fork; tool/instruction/model/owner changes; summary isolation; crash/restart; expired handlers; key revocation between calls; completed `toolUse` HTTP response versus actual disconnect; orphan writers and bounded cleanup. Prove local client tools remain local, ambient native tools/hooks/MCP cannot run, and unsupported schemas fail explicitly. Retain native endpoint controls and actual Pi wire decoding/replay proof.

See [additional Pi interface](0011-additional-pi-message-interface.md), [credential ownership](0002-subscription-credential-ownership.md), [scope](0001-gateway-enforced-loadout-scopes.md) and [explicit peer routing](0004-explicit-peer-routing.md).
