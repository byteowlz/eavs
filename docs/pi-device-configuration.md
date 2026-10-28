# Configure Pi from a connected gateway

This uses the **running gateway's key-filtered serving catalog**, unlike
`eavs models export pi`, which reads local provider declarations. Enroll a trusted
connection first using [device connect](device-connections.md). The scoped virtual
key and optional separate listener key remain in machine-local environment
variables. No inference, provider probe, OAuth refresh or credential command runs.

## Read-only provider library boundary

```sh
eavs device pi-models --name central
```

Returns schema 1 `providers` definitions suitable for Pi dynamic registration,
plus `skipped_models` and `pricing: "unverified_estimate"`. This operation writes
nothing, does not install a Pi extension, and does not modify models.json. It
exports only the selected connection's authorized chat candidates, not gateway
credentials or unrelated Pi configuration. The optional
[dynamic Pi extension/provider library](../adapters/pi/README.md) reads the same
scoped catalog directly at startup and `/eavs-refresh`, without a subprocess or
models.json write. It is opt-in, not installed automatically.

Native Chat, Responses and Anthropic APIs are used. Gateway-owned Codex routes
use the ordinary Responses client, not Pi client-side OAuth/JWT parsing. Unknown
APIs and inconsistent route paths fail closed. Models need published input/output
limits, modalities, reasoning metadata and positive input/output estimates; missing
metadata is counted as skipped, not made free. Compatibility is narrowly allowlisted.

## Explicit models.json preview and apply

```sh
eavs device pi --name central \
  --output "$HOME/.pi/agent/models.json"
```

Preview returns `revision`, `candidate_revision`, managed/replaced provider names
and skipped count. It does **not** print the merged document: your unrelated
providers might contain private keys or commands. Review the target and selected
connection, then explicitly apply both pins from that preview:

```sh
eavs device pi --name central \
  --output "$HOME/.pi/agent/models.json" \
  --expected-revision '<revision>' \
  --candidate-revision '<candidate_revision>' --yes
```

- The target must be an explicit **absolute path named models.json**. No implicit
  global/workspace edit. Choose the actual agent directory used by your Pi process;
  a bare project `.pi/models.json` is not automatically a Pi agent configuration.
- Fresh catalog authorization and candidate/file revisions are checked on apply.
  Conflicts, invalid JSON/root/provider maps, empty existing files, symlinks,
  unavailable/revoked credentials and unsupported routes leave the target unchanged.
- Only `eavs-device-<base64url(connection)>.<base64url(provider)>` is reserved for
  this connection. Other providers and root fields are preserved. A valid empty
  scope removes this namespace; an entirely unpriced/incomplete catalog is refused.
- Generated keys/headers use `${ENV_NAME}` references, never resolved values. Pi
  itself must inherit those variables. A separate `X-Eavs-Token` reference is kept
  distinct from the scoped `Authorization: Bearer ${ENV_NAME}` reference.
- Successful publication keeps `models.json.last-good`. Restore only after explicit
  review; do not blindly replay a failed/ambiguous apply. Cooperative registry and
  target locks are not a multi-file power-loss or arbitrary-editor guarantee.
- Existing dirty Git worktrees are refused by the shared local publication guard;
  no Git commit/push or automatic cleanup is performed.

For dynamic registration instead of a file, use `pi --extension /absolute/path/to/eavs/adapters/pi/extension.ts --eavs-device central`. Do not combine it with the same connection's file-managed providers: Pi file overrides can mask dynamic metadata.

Reopen Pi's `/model` picker to reload models.json. Never redirect merge output
into an input file. Existing unrelated `!commands` are preserved as inert text by
eavs; Pi may execute them later according to its own trusted configuration.

## Native desktop equivalent

The former MyGo desktop (`ui/desktop`, removed 2026-10-28) offered Connect device
and Connect Pi forms over these operations. The GPUI console (ADR-0022) will take
them over with the same rules: explicit trust and absolute target, preview separate
from confirmation, changed inputs invalidate approval, and no upstream credentials
or independent configuration writer in the GUI. Until then they are CLI-only.

## Proof

```sh
EAVS_PI_CONFIG_FIXTURES=/tmp/eavs-pi-config-fixtures just test-device
just test-pi-device-sdk /path/to/installed/pi-coding-agent \
  /tmp/eavs-pi-config-fixtures/models.json
just test-desktop-device
```

The SDK test accepts an installed package or a dependency-provisioned pi-mono
checkout. It calls Pi's actual credential-blind models parser and resolves **only
synthetic values** for generated environment references; no unrelated commands or
credentials are evaluated. Rust/Go integration uses private temporary files and a
loopback fixture server, not real inference or global Pi configuration.

Discovery/configuration
is never authorization: physical gateway admission remains authoritative.
