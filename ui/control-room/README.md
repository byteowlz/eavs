# Reusable eavs Control Room

The chosen Control Room is now a reusable module, not another themed HTML mockup. Its **mechanism** comes from the actual sibling `@byteowlz/design-system` implementation. Its graphite/lime **look** is an eavs scheme plus font/radius identity. Embedded use inherits host tokens instead.

## Implemented / not implemented

Implemented: framework-neutral mount/update/destroy, scoped Shadow DOM, graph arrangement and keyboard ports, model-specific access intents, explicit instance targets (initially **none**), isolated tenant drafts, immutable revision-pinned inventory previews, read-only and explicit offline-simulator adapters, optional Oqto inventory projection, optional React lifecycle bridge, standalone and synthetic embedded demonstrations.

**Not implemented:** production gateway grant-write coordinator, authenticated browser/backend transport, Oqto inventory endpoint wiring, installation into Oqto, credentials/rotation UI, compatibility/pricing/accounting verification, durable budget enforcement. Inventory/model presence is not authorization, readiness, billing evidence or service health. Graph and harness metadata never grant access. No real writes are possible through the supplied adapters; unsupported `live` client mode is rejected.

## Small public interface

```js
import { mountControlRoom, createReadOnlyClient } from '@byteowlz/eavs-control-room';
const room = mountControlRoom(element, {
  inventory,                         // allowlisted Inventory DTO; no credentials
  client: createReadOnlyClient(inventory), // default; deliberately no apply method
  theme: 'inherit',                  // host owns portable roles, fonts and radius
  onIntent(summary) { /* observe local intent; never automatically apply */ }
});
room.update({ inventory: nextSnapshot }); // changes reset stale intents/targets
room.destroy();                      // idempotent; removes only its own child
```

`src/index.d.ts` defines inventory, plans, pinned results, client and lifecycle contracts. A semantic inventory change (even with an unchanged version string) invalidates previews and resets all tenant drafts/targets. A semantically identical snapshot does not erase drafts. Switching tenant context preserves each tenant's isolated draft but closes/invalidate pending previews. Client replacement aborts in-flight work. Changed inventory without a replacement client safely falls back to a new read-only client.

The module accepts injected planning operations; it does not create HTTP clients or read local credential/config files. Read-only `preview` returns inventory checks, not authoritative grant eligibility. `createSimulationClient(inventory)` is an explicit alternative for an offline demo; a second eligible target demonstrates a simulated conflict, absent providers/models skip only the affected target. Results must echo the exact pinned plan/version/tenant and complete target set with closed status codes. Arbitrary error bodies are never displayed. Late responses are ignored even if an adapter ignores AbortSignal. Blocked targets cannot be represented as applied receipts. No automatic retries.

### Standalone identity

```js
import { controlRoomScheme, controlRoomIdentity } from '@byteowlz/eavs-control-room';
const room = mountControlRoom(element, {
  inventory,
  theme: { scheme: controlRoomScheme, identity: controlRoomIdentity, radius: '8px' }
});
room.setTheme('inherit'); // removes its own engine variables; host stays untouched
```

The shared engine normalizes base16/base24 and emits roles, identity and proportional radius tiers. Controls use `sm`, nodes `md`, panels `lg`, preview `xl`; 0 makes those controls sharp. Colors use emitted semantic roles; color mixing/shadows are eavs identity effects, not added portable roles. The module never modifies document/body styles, host navigation or an ambient theme provider. The host must supply the shared portable tokens for inherited mode; missing tokens are rejected with a teaching error rather than silently rendering an unthemed UI. Fonts belong to the host; the demo self-hosts Manrope/Plex with their OFL licenses.

### Optional Oqto projection

```js
import { mountOqtoControlRoom } from '@byteowlz/eavs-control-room';
const room = mountOqtoControlRoom(element, { snapshot: authorizedHostSnapshot });
room.update({ snapshot: nextHostSnapshot });
room.destroy();
```

`OqtoSnapshot` is our **host projection contract**, not a claim about Oqto's existing endpoint schema. It maps stable machine/workspace/harness IDs, parent references and separately supplied eavs instance/provider metadata. Unknown properties, credentials, commands and secret-file references are not forwarded. This adapter always inherits host styling and defaults read-only. The host must authorize inventory before projection; selecting a tenant group is not an authorization mechanism.

Actual Oqto integration also depends on its shared-token rollout (`oqto-trdk`); the sibling repo is locally named `../octo`. No Oqto code or configuration was modified here.

### Optional React bridge

```jsx
import { ControlRoom } from '@byteowlz/eavs-control-room/react';
<ControlRoom inventory={inventory} client={readOnlyClient} theme="inherit" />
```

The bridge handles mount/update/cleanup, including StrictMode and host callback rerenders. React 18/19 is an optional peer; the core is React-free. Hosts should memoize stable snapshots/clients. Supply gateway-authorized data through the projection seam; do not put a gateway master key or upstream secret in props, browser storage, graph nodes or plans. A real transport/authorization integration needs its own review and server enforcement after route resolution.

## Local build and proof

```sh
just build-control-room
just test-control-room
just check-control-room-types /path/to/existing/typescript/bin/tsc
python3 ui/control-room/serve.py --port 5281  # separate terminal
just test-control-room-browser
```

No install is performed. Bun bundles against `../design-system/impls/shadcn-ts/src/index.ts`; override `DESIGN_SYSTEM_DIR` with another local checkout if needed. There is no copied or approximated theme engine. `dist/build-manifest.json` identifies the engine version and source SHA-256; this is local workspace integration, not a published/pinned registry release. The frontend package matches eavs 0.8.3 and is private. `dist/` is generated and ignored; rebuild before consuming the local package.

An optional React proof builds against explicit **already-existing** package directories, never downloading/installing them:

```sh
REACT_PACKAGE_DIR=/path/to/existing/react \
REACT_DOM_PACKAGE_DIR=/path/to/existing/react-dom \
SCHEDULER_PACKAGE_DIR=/path/to/existing/scheduler \
  bun run ui/control-room/tests/build-react-proof.js
python3 ui/control-room/tests/browser.py --react-proof
```

That optional bundle is verification-only, separate from the core/React exports; React and React DOM licenses are included beside its public assets.

Tests cover pure schema/plan/client projection, explicit scopes, missing models/providers, pinned revisions, private error/credential fields, multiple independent mounts, inherited themes/base16/radius, host style/navigation preservation, async cancellation and late-result guards, native keyboard, drag, cleanup/remount, mobile flow and absence of fetch/XHR/WebSocket/non-GET requests. Optional React proof exercises actual React StrictMode with host state updates and unmount/remount.

## Tailnet demonstration

The read-only host serves a fixed allowlist under `dist/`, binds loopback 5281, denies source paths/listings/write methods and uses CSP `connect-src 'none'`. Standalone is `/`; synthetic host embedding is `/embedded.html`. Existing five-mockup gallery remains untouched.

Current hosting: `http://arch-dev-01.example.com:8800/` (standalone) and `/embedded.html` (synthetic Oqto shell). Transient user service `eavs-control-room` listens at loopback 5281, with a separate tailnet-only Serve route on **8800**; no Funnel. Existing ports 8787/8788/8789 remain unchanged. Teardown: `tailscale serve --http=8800 off` and `systemctl --user stop eavs-control-room.service`. No automatic restart installation after reboot. Stop only this service/its explicitly assigned Serve port when finished, leaving other routes intact.
