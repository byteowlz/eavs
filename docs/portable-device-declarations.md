# Portable device declarations

`eavs device portable` exposes shared Rust operations in `src/device/portable.rs`.
These operate on declaration files and honest selection plans only. **They do not
change effective serving routes, grant authority, enroll a principal, fail over
inference or replicate accounting.** Existing Pi configuration operations are separate.
Portable operations do not load the device connection registry.

## Explicit reviewed file workflow

```sh
# Validate/inspect an explicitly chosen file (no referenced secrets are opened).
eavs device portable inspect --declaration ./reviewed.json
# Writes nothing; returns review.expected_revision and review.candidate_revision.
eavs device portable preview --candidate ./reviewed.json --output ./device.json
# Use both exact preview pins. Changed bytes require new human review.
eavs device --candidate-revision CANDIDATE_SHA portable apply \
  --candidate ./reviewed.json --output ./device.json \
  --expected-revision CURRENT_SHA --yes
```

There is no automatic retry after a conflict. Do not regenerate preview silently on
apply failure. Optional distribution is explicit file copying or a separately reviewed
manual Git workflow; resolve dirty edits/merges yourself. These operations never invoke
Git, repository hooks/config, commit, pull or push, nor certify repository cleanliness.

## Strict schema v1

```json
{
  "version": 1,
  "providers": [{"id":"remote","origin":"https://gateway.example","connectivity":"public"}],
  "routes": [{"id":"chosen","provider":"remote","model":"model-name"}],
  "loadouts": [{"id":"work","routes":["chosen"]}],
  "profiles": [{"id":"laptop","loadout":"work"}]
}
```

All objects reject unknown/duplicate fields. Required fields have no defaults.
Documents are bounded to 256 KiB; each collection and loadout selection to 128 items.
Identifiers/models are nonempty ASCII tokens, at most 128 bytes; references must
resolve, identifiers and selections must be unique. Provider origins are at most 512
bytes, with no userinfo, query, fragment or non-root path. HTTPS is mandatory except
explicit `local` loopback HTTP. `local` origins must be loopback (including localhost);
LAN services use public or tailnet plus HTTPS. Operator-declared connectivity classes
are not verified network membership, authority or billing policy.

Credential commands, file references, literal credential fields, virtual keys,
arbitrary provider options and ledgers have no schema slots. Syntax cannot recognize
a secret deliberately disguised as an identifier or environment variable name: review
before sharing. Rejected-input parse errors redact content; never echo raw rejected bytes.

## Machine-local bindings

```sh
# Preview an explicit provider -> environment variable NAME binding; no value lookup.
eavs device portable bind --declaration ./device.json \
  --output ./machine-local.json --provider remote --env EAVS_REMOTE_KEY
# Publish using all three pins from that preview.
eavs device --candidate-revision BINDING_CANDIDATE_SHA portable bind \
  --declaration ./device.json --declaration-revision DECLARATION_SHA \
  --output ./machine-local.json --provider remote --env EAVS_REMOTE_KEY \
  --expected-revision BINDING_CURRENT_SHA --yes
```

Bindings must be a separate explicit machine-local file, excluded from distribution.
Bind preserves other valid bindings, replacing only the explicitly named provider.
There is no implicit credential import or environment read, even on apply. Provider
IDs must resolve, references must be uppercase environment variable names (digits
allowed after the first character), bounded to 128 bytes. Commands, `${...}`, secret
paths, literal-value fields and malformed existing binding documents are rejected.

```json
{"version":1,"bindings":[{"provider":"remote","env":"EAVS_REMOTE_KEY"}]}
```

Bindings use the same preview/CAS/last-good publisher as declarations. Declaration
revision must also match the binding preview at apply. This is a snapshot check, not
a cross-file transaction; concurrent declaration changes after the final check are
not fenced. Binding names and declaration metadata confer no serving authorization.

## Explicit observations and selection plans

```sh
# Explicitly approve 1..8 provider IDs for HEAD requests to their declared origins.
# Only stdout is produced; choose any persisted observation file yourself.
eavs device portable observe --declaration ./device.json \
  --provider remote --ttl-seconds 30 > ./observations.json
# No network checks happen while planning. Repeat --route for explicit alternatives.
eavs device portable plan --declaration ./device.json --profile laptop \
  --route chosen --observations ./observations.json
```

Observation sends **HEAD to the root only**, without credentials, proxy, redirects,
retries, response-body reads or inference. TLS verification is enabled. Per-request
connect/total timeouts are 2 seconds; at most 8 sequential requests and a 17-second
aggregate deadline. Any HTTP status (including 401, 302 or 500) means only an HTTP
response was observed; transport/TLS/DNS failure is `unreachable` for that provider,
**not evidence of a disconnected Tailscale/VPN**. No provider probe is used.

Observation documents pin canonical validated declaration content, including origins,
and carry per-provider Unix-second timestamps. TTL must be 1..60 seconds (default 30).
Plan checks current time: observations older than TTL or in the future become `unknown`.
Changed declaration content, duplicate/unknown providers or invalid TTLs reject the
observation document. Missing observations are unknown. Clock correctness is a local
assumption; a forged observation document is not trustworthy health evidence. None of
this metadata is admission or authentication evidence.

Plan requires an explicit profile and one or more unique route IDs present in its
loadout. It returns exactly those choices in caller order—no auto-selection, retries,
request replay, automatic paid alternatives or serving writes. Output says
`kind: declaration_selection_only`, `serving_changed: false`, `authority_granted: false`.
`reachable` is transport metadata, never proof of model availability, credentials,
budget or principal scope. No actual live failover is implemented.

With internet up and a tailnet endpoint unreachable, declarations are retained; an
independently selected reachable public/local route remains listed as reachable. If
only the tailnet route is selected, no public/paid alternative is added. Each device
can inspect/plan offline without a central server, and must establish its own local
credential references and independent enrollment where needed.

## Rust operation boundary

```rust,ignore
use crate::device::portable;
let declaration = portable::parse(&candidate)?;
let review = portable::preview(&destination, &candidate)?;
// Explicit human review; preserve both pins, never silently re-preview on conflict.
portable::apply(&destination, &candidate, &review)?;
let binding_review = portable::preview_bindings(&local_path, &binding_bytes, &declaration)?;
portable::apply_bindings(&local_path, &binding_bytes, &binding_review, &declaration)?;
let observations = portable::observe(&declaration, &explicit_provider_ids, 30).await?;
let selections = portable::plan(&declaration, profile, &explicit_route_ids, &observations, now_unix_seconds)?;
```

Shared binding functions accept full strict reference-only documents; the CLI adds
single-provider merge and declaration-pin checks. Serving code must independently
resolve effective principal scope if real selection is ever integrated.

## Storage and executable proof

Publication uses a sibling `.portable.lock`, validated current bytes, SHA-256 CAS,
same-directory temporary-file fsync/atomic rename, and `.portable.last-good` retaining
validated prior bytes. A lock requires operator investigation, not automatic stale-lock
removal. Empty/malformed current files stop publication. Unix tempfile permissions are
private by default. Use trusted directories: checks are cooperative, not race-free
against hostile writers/parent symlinks. No directory fsync, power-loss guarantee,
Git cleanliness certification or multi-file transaction is claimed.

`CARGO_INCREMENTAL=0 just check` compiles the real CLI. `CARGO_INCREMENTAL=0 just
test-device` runs portable CLI tests plus existing device/Pi regressions. Portable
fixtures cover malformed/oversized/secret rejection and redaction, candidate/current
CAS conflicts, binding publication/last-good, explicit profile/routes, stale/future
observations, TTL/origin pinning, internet-up/tailnet-down planning, synthetic HEAD
requests without auth/redirect following, and transport failure classification.
Tests use loopback servers or offline fixtures only. Tracked with proof in `eavs-gam7`.
