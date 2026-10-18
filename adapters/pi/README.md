# Pi integration

Two opt-in ways to use the **running gateway's scoped catalog**:

1. Native models.json preview/apply: `eavs device pi --name central --output /absolute/models.json`.
2. This dynamic provider extension: no models.json file to maintain.

Both share equivalent, cross-tested credential-blind native provider projections.
The older `eavs models export pi` remains a local-declaration export, not scoped
running-instance discovery. Existing native and additional Pi message APIs remain
available; this integration uses native APIs rather than replacing them.

## Dynamic connection, no config write

First connect explicitly through `eavs device connect` or the native desktop. The
registry records a trusted origin and **environment variable names**, not key
values. Keep a scoped virtual key and optional listener credential in those
machine-local variables. Do not put keys in shell history or share credential files.

Try the extension for one invocation (replace the path):

```sh
pi --extension /absolute/path/to/eavs/adapters/pi/extension.ts \
  --eavs-device central
```

Or set `EAVS_DEVICE_NAME=central` for the Pi process. An explicit
`--eavs-device-config /absolute/device.json` overrides the machine-local default
`$XDG_CONFIG_HOME/eavs/device.json` / `~/.config/eavs/device.json` (Windows APPDATA).
Relative overrides, unsafe origins, credential commands, duplicate/malformed
profiles and symlink registry files are refused. No auth.json or referenced secret
file is read. `${ENV}` references are resolved only from the selected profile's
local environment. Listener auth remains separate from scoped Bearer auth.

Pi awaits the bounded catalog read in the async extension factory, so models are
registered before startup selection and `--list-models`. The factory starts no
subprocess, watcher or service. HTTPS verifies certificates; literal loopback HTTP
requires the profile's explicit approval. No environment proxy, redirect, retry,
provider probe or inference request is used. `PI_OFFLINE=1` suppresses startup
network discovery; an explicit `/eavs-refresh` remains a user-initiated read.

Node is the production runtime (tested with Node26.8.1). It uses private HTTP(S)
agents with proxy environments disabled and explicit TLS verification. Tested
Bun1.3.13 ignores fetch proxy-bypass values; its optional transport therefore
**refuses any configured proxy environment**, rather than forwarding a virtual key
or mutating shared environment variables. Pure projection works in either runtime.

Run `/eavs-refresh` to update models. A valid empty scope removes only this
extension's registrations; transport/auth failures retain last-known metadata with
a warning, **not continued authorization or a fallback choice**. The gateway still
checks the effective principal/route on inference. Shutdown aborts pending reads
and removes only owned registrations. Models are not automatically selected.

Choose file-based **or** dynamic configuration for a connection. Do not leave its
same `eavs-device-*` providers in models.json while enabling the extension: Pi's
file overrides can mask dynamically refreshed metadata. Neither approach installs
or changes the other implicitly. Persistent `pi install /absolute/.../adapters/pi`
is an explicit user action, not performed by eavs.

## Shared library and limits

`provider-library.ts` exports the bounded profile reader, authenticated catalog
transport and pure `projectPiProviders`. `live-providers.ts` owns replace/prune/
abort/lifecycle behavior independently of Pi UI. An embedding harness can inject
this projection rather than maintaining a configuration file. No library function
starts inference, issues authority, copies upstream secrets or implements budgets.

Only priced/capability-complete chat candidates are exported; unsupported or
inconsistent native routes fail closed, incomplete models are counted as omitted,
and price values remain **unverified estimates**, not durable settlement. Gateway
Codex uses ordinary Responses, leaving subscription OAuth/JWT handling with eavs.
No image/classifier discovery, full vendor conformance or automatic polling.

## Verification

```sh
just test-pi-adapter
just test-pi-device-library
EAVS_PI_CONFIG_FIXTURES=/tmp/eavs-pi-config-fixtures just test-device
EAVS_PI_CONFIG_FIXTURES=/tmp/eavs-pi-config-fixtures just test-pi-device-library
just check-pi-extension-types /path/to/typescript/lib/tsc.js /path/to/installed/pi-coding-agent
```

Tests cover malformed/hostile metadata, pricing, scope pruning, literal credential
references, exact Rust/TS projection parity, listener separation, response bounds,
redirect rejection, abort, startup/refresh/shutdown and private errors. The actual
Node transport is exercised separately with an unusable proxy environment and
self-signed TLS (even with ambient TLS validation disabled). These synthetic tests
require Node with TypeScript stripping support and OpenSSL already installed. No real
credentials/inference/global config edits. Actual Pi models parsing/interpolation
is checked separately with `just test-pi-device-sdk`.

Strict type proof uses Node + TypeScript5.9.3, Node types22.19.19, installed Pi1.1.0
public declarations and its optional Gemini DTS MCP peer1.25.2, provisioned only in
an isolated temporary dependency directory. No `skipLibCheck`/ambient type stub.
The recipe takes explicit existing tools and does not install them.

See [device/Pi contract](../../docs/pi-device-configuration.md), ADR0015 and
`eavs-p9jr`. This source is not an installed or released package.
