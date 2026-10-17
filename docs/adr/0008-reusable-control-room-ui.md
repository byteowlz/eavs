# Reusable Control Room over shared design-system and planning interfaces

Status: Accepted direction; reusable planning module and offline demonstrations implemented (`eavs-0rw4`). Production gateway grant-write and actual Oqto host wiring remain unimplemented.

## Context

The graph-admin studies selected Control Room. A standalone HTML mockup with bespoke theme variables would duplicate UI and styling when embedded in Oqto. Eavs must remain independent, and harness/workspace context must never become authorization. ADR0003 already establishes adapters over shared provisioning operations; it does not prove those operations are implemented.

## Decision

Keep a framework-neutral, lifecycle-owned module in `ui/control-room`, with a small mount/update/destroy interface, immutable allowlisted inventory DTOs and injected planning clients. Standalone eavs owns its shell. An optional Oqto projection and optional React lifecycle bridge mount the same module without an iframe or duplicated UI. Oqto remains optional; no Oqto/Pi runtime dependency enters the core.

Use the actual `@byteowlz/design-system` engine from the sibling workspace, consuming its portable roles, identity fonts and proportional radius tiers. Eavs owns its graphite/lime scheme; embedded mode inherits host variables without mutating document/body styles or host navigation. The local build fingerprints the shared engine source. This is not a registry publication or pinned production dependency rollout; Oqto token migration remains separately tracked (`oqto-trdk`).

Inventory relationships and graph bindings describe context and policy intent, not deployed authorization. Targets start empty and are explicitly selected. Tenant contexts isolate drafts. Preview pins the inventory version, target set, model-specific intents and each target's snapshot revision. These snapshot revisions are not credential-rotation CAS tokens. Changed contexts/intents/targets/client/snapshots cancel or invalidate asynchronous work; late results are discarded even if cancellation is ignored. No automatic retry or cross-instance credential replication.

Ship only inventory-read-only and explicit offline-simulator adapters in this slice. The client interface contains no live apply, credential, budget or provider-configuration operations. A future real coordinator requires verified server APIs and authorization after route/alias/fallback/policy resolution, independent per-instance outcomes and explicit recovery semantics. A successful inventory check does not establish protocol compatibility, pricing, accounting, health or permission.

## Verification and limits

Pure tests exercise schema projection, physical models, target scope, immutability, missing declarations/models, snapshot conflict, result pinning and removal of unknown credential/error fields. Browser tests cross the public lifecycle interface and prove independent mounts, inherited base16/base24 themes/radius, preserved host styles/navigation, escaped labels, stale-result guards, cleanup, keyboard/drag and mobile behavior. An optional test bundle checks the React bridge with actual React StrictMode and host callback rerenders. No paid inference, provider probes, imported credential commands or remote gateway mutations are performed.

The synthetic Oqto shell is an embedding demonstration, not deployment into Oqto. Real inventory transport, scoped administration authentication, authoritative grant application, compatibility discovery and durable accounting require separate implementation and evidence.

## Alternatives

- Copy the graph UI into Oqto: duplicates behavior, ownership checks and visual fixes.
- Iframe the standalone page: introduces a second navigation/theme/credential surface and weakens host integration.
- Require Oqto/React in the core: makes independent eavs instances depend on an orchestrator unnecessarily.
- Copy theme-engine outputs into bespoke constants: breaks the shared mechanism and radius guarantees.
