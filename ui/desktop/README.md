# Eavs Connection Desk — native desktop draft

MyGo v0.3.2 native `ui.View`, canonical design-system adapter, no HTML/webview.
Calls shared Rust **device operations**, not a parallel routing/credential store.
Source is buildable and under verification, not installed or release-ready.

## Working surface

- Explicit metadata refresh, connection selection, distinct auth/network/server/
  rate-limit diagnostics and candidate model filtering.
- **Connect device**: trusted gateway, scoped-key and optional separate listener
  **environment variable names**, preview, then explicit pinned confirmation.
- **Connect Pi**: selected connection, explicit absolute models.json target, scoped
  preview, then candidate/revision-pinned publication. Unrelated providers survive.
- Approval is bound to unchanged inputs; target/key-reference/URL changes cannot
  reuse it. URL/key-reference edits reset trust. Failures clear approval, never
  retry automatically, never display private stderr or raw provider replies.
- Bounded subprocess output/deadlines, cancellation, stale generation rejection,
  visibly last-known data and close cleanup. No inference, secret replication,
  implicit enrollment/fallback, or service lifecycle changes.

The window starts without network/process work. Use Refresh or an explicit form.
Closing the GUI does not stop a gateway service. A cancelled apply may have
published before its reply was lost: inspect state and preview again.

See [device CLI](../../docs/device-connections.md) and
[scoped Pi configuration](../../docs/pi-device-configuration.md).
The web Control Room remains separate and is not embedded here.

## Offline checks and build

```sh
just test-desktop-control
just test-desktop-device     # builds real Rust CLI, synthetic loopback workflow proof
just check-desktop-native   # native tester/race/vet/build, no implicit downloads
```

Native MyGo requires **Go 1.27.1** and cached pinned modules. Recipes use
`GOTOOLCHAIN=local GOPROXY=off GOSUMDB=off`; they never provision prerequisites
implicitly. At the current source checkpoint Go1.27.1/MyGo0.3.2 were fetched into
`/tmp/eavs-native-build-cache` only, without changing the host's global Go1.27.0:

```sh
# After explicitly provisioning a pinned Go toolchain and dependencies:
PATH="/path/to/go1.27.1/bin:$PATH" \
  GOMODCACHE=/path/to/pinned-module-cache \
  GOCACHE=/path/to/build-cache just check-desktop-native
./ui/desktop/build/eavs-desktop --eavs-binary /absolute/path/to/source-built/eavs
```

Select a binary implementing the current device operations. An older installed
binary produces a private failure, never a synthetic substitute. The standard-
library control module still works with Go1.27.0.

## Actual native window proof (Linux)

```sh
just test-desktop-window /tmp/eavs-native-window-proof
```

Uses existing Xvfb/ImageMagick/X11/XTest, an isolated empty home, disabled inspector,
no inherited credentials, and an intentionally nonexistent eavs executable. It
captures the actual window and keyboard-opened enrollment form; no CLI/network
operation is performed. Inspect `connections.png`, `enrollment.png` and the proof
log. On this host Xvfb needs the Mesa EGL vendor override used by the script;
NVIDIA EGL otherwise crashes the **X server**, not the application.

Native package tests/race checks and actual X11 window/keyboard captures now pass.
The window reports software rendering: this is **not hardware-GPU proof**.

## Provenance and remaining boundaries

- MyGo v0.3.2 / `88543ba0bc7e131ef00126a56a627c25c735bec6`.
- Canonical adapter is an unchanged licensed snapshot from
  `templates/go-mygo/third_party/mygo-go`, template revision
  `670710a05949db7fac786701a237e7a254f14bc5`; see `PROVENANCE.md`.
- Native composition is a draft, not inherited template/Studio product approval.
- Invitation redemption UX, key issuance/rotation, provider/loadout editors, tray/
  menu lifecycle, packaging, actual hardware GPU, Mac/Windows runtime and OS screen-
  reader verification remain separate work. Native tester announcements/keyboard
  assertions do not certify platform accessibility.
- Metadata reachability is not upstream readiness/quota or Tailscale-state evidence.
  This GUI does not infer fallback authority or make remote credentials local.

[ADR0014](../../docs/adr/0014-native-desktop-device-client.md) · `eavs-wnsh`.
