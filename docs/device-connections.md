# Explicit device connections (schema v1)

The CLI is the shared desktop operation surface. It does not need a local gateway
process. Configure a central gateway once, then explicitly enroll each client.
Tailnet reachability is **not authorization**. Provision an independently revocable,
provider/model-scoped virtual key on the central instance using existing key
administration. The device flow neither issues nor attenuates keys. The scoped
`GET /v1/eavs/catalog` endpoint must be enabled and reachable. Do not supply a
listener/master credential or an upstream API credential as the scoped key. If
the listener separately requires `X-Eavs-Token`, use the distinct
`--listener-key-env EAVS_LISTENER_TOKEN` reference; it never substitutes for the
virtual key. Missing/invalid referenced listener values fail before HTTP.

## Enrollment

Provide the virtual key through a machine-local environment variable (using your
secret manager; do not paste keys into arguments/history). Only environment
variable **names** are persisted. No credential command or referenced secret file is
read. Inherit the same variable when launching the desktop/CLI.

```sh
eavs device connect --name central --url https://central.example \
  --key-env EAVS_CENTRAL_KEY --trust
```

This previews the complete proposed connection registry as JSON, after a successful
scoped catalog read. To apply, repeat the exact command with `--yes`,
`--expected-revision <revision>` and `--candidate-revision <candidate_revision>`
from that preview. `enroll` is an alias for `connect`. Reusing a name explicitly
replaces that connection; there is no implicit discovery, key replication or routing.
Neither preview nor failure modifies the registry.

Default registry: `$XDG_CONFIG_HOME/eavs/device.json` (platform config-dir fallback).
Override with `eavs device --config /absolute/path/device.json ...`. It is a strict
JSON object with `connections`, each containing `name`, `url`, `key_env`, and
`allow_loopback_http`, plus optional `listener_key_env`. Missing registry means no connections; malformed, empty,
unknown-field, duplicate, oversized or invalid registries fail closed. Maximum 32
connections. Keep this file in a trusted, machine-local directory outside Git.

HTTPS origins only: no userinfo, query, fragment or URL path. Plain HTTP requires
`--allow-loopback-http` and a literal loopback address, for explicit local testing.
No proxies (including environment proxies), redirects, automatic retries or TLS
verification bypass. Per-connection connect timeout 2s, request timeout 5s and
whole observation deadline 6s; connections are observed sequentially. Catalog body
limit 4 MiB, 256 providers and 4096 models. Discovery never calls `provider_probe`
or performs inference. A ready catalog is not proof of inference admission,
upstream health, entitlement, remaining budget, or a working Tailscale session.

## Desktop contract

```sh
eavs device status --json
```

```json
{"schema_version":1,"connections":[{"name":"central","url":"https://central.example","state":"ready","diagnostic":"catalog_available","providers":[{"id":"public","models":[{"id":"model","name":"Model"}]}]}]}
```

States and diagnostic codes:

| State | Diagnostic | Meaning |
|---|---|---|
| ready | catalog_available | Scoped live catalog read succeeded |
| unconfigured | credential_missing / scoped_key_required | Missing local env value or not a virtual-key-shaped value |
| unconfigured | listener_credential_missing_or_invalid | Explicit listener capability reference is unavailable/invalid |
| unreachable | transport_failure / transport_timeout | DNS/TLS/connect/body transport failed or timed out |
| unauthorized | authentication_rejected | Gateway returned 401/403 |
| unavailable | rate_limited | Gateway returned 429 |
| unavailable | server_failure | Gateway returned 5xx |
| unavailable | unexpected_http_status | Other status, including redirect |
| unavailable | invalid_catalog / catalog_limit | Unsupported/malformed/oversized catalog |

No raw upstream body, credential, credential reference, transport error string,
provider config, pricing or arbitrary catalog metadata appears in status. Public
provider/model IDs and display names remain operator-controlled metadata, not a
claim that malicious operators cannot place sensitive text in a name. `providers`
is empty unless ready; cached inventory is never presented as live.

Status exits 0 for a completed observation, including unhealthy connections. Invalid
configuration/operation errors exit 1 with a JSON error envelope on stdout
(`schema_version:1,error:{code:"device_operation_failed",message:...}`) and a brief
stderr error. Clap usage errors exit 2. Without `--json`, status prints concise rows.
Connect and sync always emit JSON; their successful preview/apply responses contain
`applied`, `revision`, and `candidate_revision` in schema v1.

## Connectivity-aware inventory sync foundation

```sh
eavs device sync --name central
```

Preview a sanitized scoped declaration **inventory**, then repeat with `--yes`,
`--expected-revision` and `--candidate-revision`. Saves sibling
`device-inventory-central.json` containing schema_version, name, url and the same
public provider/model projection. This is not a provider configuration export and
is never imported into serving routes or harness configuration. Schema/identity
conflicts, disconnected/auth/rate/server failures leave the current snapshot intact.
No connectivity event changes a configured route or initiates paid fallback.

Local writes use a cooperative exclusive lock, current-byte CAS, candidate pin,
same-directory temporary publication and a `.json.last-good` backup. A dirty Git
working tree at the target directory rejects apply (no pull/push/stage). Locks are
not automatically stolen; inspect a stale `.json.lock` manually after a crash.
These are single-file operations, not distributed transactions. Arbitrary external
writers ignoring the lock, parent-directory symlink attacks, multi-file atomicity,
filesystem/power-loss guarantees and automated recovery are not promised. A failed
post-backup write can update the backup while retaining the current file. Inspect
both before manual recovery.

## Deliberately outstanding

- Central listener deployment, tailnet ACL/TLS setup and scoped-key bootstrap,
  expiry/rotation/revocation UX remain operator responsibilities.
- Actual shared provider/loadout declaration exchange and binding to explicitly
  approved machine-local credential references require a reviewed schema. This
  slice syncs inventory only; it does **not** make central routes usable offline.
- Existing independently credentialed local public providers remain untouched and
  can continue to work with internet when Tailscale is disconnected. No automatic
  failover or paid-route substitution is added or guaranteed.
- No full mesh, delegated grant protocol, upstream credential replication, Claude
  SDK integration, Pi export or harness merge writer. The separate Pi adapter
  merge-data-loss bug `eavs-r833` is now source-fixed and offline-tested; device
  sync still does not call that writer or configure a harness.
- Transport failure is not attributed specifically to Tailscale: DNS, TLS and
  gateway loss are indistinguishable at this boundary. Upstream auth/rate/server
  failures during inference are not tested by catalog observation.

## Synthetic proof

`just test-device`: loopback HTTP catalog fixtures, classification, redirects,
bounded bodies, secret-field projection, explicit enrollment preview/apply, live
status, separate listener/scoped-key headers and private missing-reference refusal,
inventory sync and offline retention, malformed configuration, CAS,
candidate pins, last-good preservation, symlinks, lock and dirty-repo conflicts.
No real key, secret store, provider, inference, install or service restart required.
