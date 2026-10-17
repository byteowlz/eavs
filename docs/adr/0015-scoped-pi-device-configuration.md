# Scoped Pi device configuration and dynamic registration

Status: Proposed, source implementation under verification (`eavs-p9jr`).

## Decision

A trusted device connection reads the authenticated running gateway's scoped
catalog, not the local unscoped provider declaration export. Share one projection
between read-only `device pi-models` (dynamic provider registration) and explicit
`device pi` (models.json preview/apply). Neither operation performs inference.

Use physical native API routes, positive input/output price estimates, explicitly
published limits/modalities and a narrow compatibility allowlist. Reject unsupported
or inconsistent routes, not arbitrary server-supplied URLs/headers. Codex behind
the gateway uses Pi's ordinary Responses client; gateway-owned OAuth must not be
parsed/refreshed as a client-side virtual key. Preserve separate scoped Bearer
and optional listener credentials as `${ENV}` references only.

A file writer requires an explicit absolute `models.json` target. Each connection
owns only `eavs-device-<base64url(connection)>.<base64url(provider)>` entries.
Rebuild that namespace from a fresh observation, preserve all unrelated providers
and root properties, and refuse malformed/empty existing files. Valid empty scope
removes that namespace; incomplete all-unpriced catalogs cannot synthesize free
models. Preview shows bounded metadata and opaque pins, never the merged document
which may contain unrelated private keys or command strings. Do not execute those
strings. Apply requires current/candidate pins, refetches authorization/catalog,
holds the cooperative device registry lock, and atomically publishes the single
harness file with a last-good backup. No unrequested user/global config write.

An opt-in Pi extension can register this same credential-blind projection at
startup and explicit refresh without maintaining models.json. It owns only the
names it registered. Discovery and harness configuration never grant authority:
physical gateway admission still enforces the current key and effective route.

## Limits

Prices are unverified estimates, not settlement. Config-file commands preserved
for unrelated providers remain executable by Pi itself if the user enables them;
eavs never evaluates them. Single-file publication/cooperative locks do not promise
cross-process power-loss recovery or fences against arbitrary external editors.
No inferred upstream health, inference replay, automatic billing fallback,
credential distribution, global config install, or extension installation.

## Verification

Synthetic catalog/refetch, revocation, inconsistent route, malformed config,
namespace/pruning, stale candidate/file/registry and secret-safe receipt tests;
actual Pi models parser and synthetic environment interpolation; native device
and unrelated native API regression suites. See `docs/pi-device-configuration.md`.
