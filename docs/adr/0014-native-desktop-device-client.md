# Native MyGo desktop over shared device operations

Status: Bounded implementation under verification; native composition is a draft. Native compile/race/vet and actual software-rendered X11 window/keyboard proof pass; hardware GPU, packaging, platform and OS accessibility proof remain open.

## Decision

Keep gateway policy, scoped discovery and device registry mutations in Rust eavs. A native MyGo v0.3.2 `ui.View` application presents shared `eavs device status --json`, connection enrollment and scoped Pi configuration preview/apply through a bounded subprocess JSON adapter. This is a process boundary, not direct Go access to Rust internals. It is not a webview, frontend server or parallel inference implementation.

Use the unchanged canonical MyGo design adapter from the native template, copied with its provenance/license and a portable local replacement. Do not invent another palette or treat source token parity as product composition approval. Source references: MyGo v0.3.2 tag `88543ba0bc7e131ef00126a56a627c25c735bec6`; templates `670710a05949db7fac786701a237e7a254f14bc5`; adapter's own `ui/desktop/third_party/mygo-go/PROVENANCE.md` records its source.

The initial surface was read-only. It now also provides Connect device and Connect Pi forms over the same Rust CLI operations: explicit gateway trust and local environment references, selected connection and absolute target, preview, then unchanged-form revision/candidate confirmation. URL/key-reference edits reset trust, failures clear approval, close/cancel invalidates late results. A cancelled apply may already have published; inspect state before retrying. The GUI does not itself issue invitations/keys, store credential values, start/stop services, install dependencies or select fallback routes. No decorative nonfunctional Apply buttons.

## Safety/lifecycle

- Execute an explicitly selectable eavs binary with a fixed argument list, never a shell or remote-supplied arguments. The binary and its inherited environment are machine-local trust inputs. No silent fixture fallback.
- Bound stdout to 4 MiB, suppress stderr/private process errors, set an operation deadline and cancellation; project only known status/catalog fields. Origins with userinfo/query credentials and control-bearing labels are rejected.
- `ready` means current scoped catalog metadata is reachable, not that an upstream provider is healthy, paid inference is authorized now or quota is available. Transport failure is not definitive Tailscale-state evidence. Rate limits, authentication and server failures remain distinct diagnostics.
- Preserve last-known data on errors, visibly mark it stale, never infer routes were deleted. Do not automatically select another connection.
- Background reads and writes update models only through `Window.Update`; request generations reject stale completions and window close cancels outstanding work. No ui.Element escapes a build pass.
- Desktop refresh reads the Rust machine-local credential reference deliberately; the GUI itself neither imports nor copies upstream credentials. Native text remains data, never executable markup.

## Verification and boundaries

`ui/desktop/control` is a separate standard-library-only Go 1.27.0 module, permitting parser, bounded executable, timeout, private-error, stale-result and lifecycle verification without fetching a toolchain or UI dependencies. Its `eavs-desktopctl` uses the same adapter as the native view. Native UI tests use `ui.NewTester` and injected device operation results, never real gateways.

MyGo and the canonical adapter require Go 1.27.1. The host's global Go remains 1.27.0; pinned Go1.27.1/MyGo0.3.2 dependencies were provisioned only in a temporary isolated cache. Native race-enabled tester/vet/build now pass. An actual isolated X11 window and keyboard-opened form were captured using software rendering, an empty home and no inherited credentials. Real compiled Rust CLI/Go workflow integration verifies connection/Pi preview/apply against synthetic loopback catalogs. Do not infer hardware-GPU, cross-platform, packaging/tray, invitation UX or OS screen-reader certification from these proofs.

See [explicit device connections](0013-explicit-device-connections.md), [scoped serving catalog](0010-scoped-serving-catalog.md) and [shared operations](0003-harness-adapters-and-shared-operations.md).
