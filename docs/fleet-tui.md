# Fleet provider administration

`eavs tui` is a local operator interface for independent eavs instances. It uses
the same fleet client as `eavs fleet`; it is not a central service and does not
require Pi, Oqto or a running xlatch application.

## Start

1. `eavs fleet init` prints a secret-free TOML registry template to stdout.
   Save/edit it as `~/.config/eavs/fleet.toml` (or
   `$XDG_CONFIG_HOME/eavs/fleet.toml`). Do not overwrite an existing registry.
2. Supply each `admin_key_env` through your local secret manager/environment.
   Never put key values in the registry or command-line arguments.
3. Run `eavs tui`, or `eavs tui --fleet /path/to/fleet.toml`.
   `EAVS_FLEET` also selects the registry; explicit flags take precedence.

Example registry: [examples/fleet.toml](../examples/fleet.toml).
Instances need this version's `/admin/providers/sources` and create-only endpoint.
Old servers report unavailable; the client does not fall back to unsafe upsert.

## Keyboard

| Key | Action |
| --- | --- |
| `Tab` | Switch instance/provider focus |
| `j`/`k`, arrows, `Home`/`End`, `g`/`G` | Move cursor |
| `Space`, `Ctrl-A` | Toggle/select instances independently of cursor |
| `r` | Refresh inventories (read-only, four concurrent requests) |
| `a` | Add provider form: name, type, upstream URL, local key ENV reference |
| `c` | Rotate selected provider's credential |
| `u` | Preview rollback of selected provider's last credential revision |
| `:` / `Ctrl-P` / `?` | Fuzzy command palette |
| `Enter` in form | Preview, **not apply** |
| `Enter` in preview | Apply eligible targets independently |
| `Esc` | Cancel form/preview or close palette |
| `q` / `Ctrl-C` | Quit once active bounded requests have settled |

Without multi-selection, actions target the focused instance. With selection,
those named instances are the targets. The same explicitly chosen local provider
key reference is used for each selected target; no key is fetched from one machine
and copied to others. Missing/unavailable/TOML-managed targets are skipped and
reported rather than silently changed. Review all targets with preview scrolling.

Credential revisions stay pinned from preview through confirmation; stale writes
fail. Provider add uses create-only semantics, so a concurrent declaration cannot
be overwritten. Results are per-instance; batch actions are not distributed
transactions. A lost response can mean unknown acceptance—refresh before deciding
whether to retry. Requests never retry a conflict automatically.

## CLI parity

```sh
eavs fleet list --json
eavs fleet rotate openai --instance dev-east --key-env NEW_OPENAI_KEY
# Preview only; upstream key value is not read until apply:
eavs fleet rotate openai --instance dev-east --key-env NEW_OPENAI_KEY --yes
```

`add` takes `--provider-type`, `--base-url` and `--key-env`; repeat `--instance`
for explicit batch targets. `rollback` needs the provider name and target instances.
Mutation commands require explicit target flags. Without `--yes`, they print a
preview. A separate `--yes` invocation builds a fresh plan; it does not replay a
saved earlier preview. Use the TUI for confirmation of the exact displayed plan.
`--fleet PATH` precedes the subcommand; `list --json` emits per-instance results.
Failures produce a nonzero exit code after reporting independent results.

## Boundaries

- HTTPS required, except conventional localhost/literal loopback HTTP tunnels.
  Redirects and environment proxies are disabled; URL userinfo/query/fragment are
  rejected. Reverse-proxy base paths are preserved. Requests have bounded timeouts
  and response bodies (including responses without Content-Length).
- Registry loading/preview does not execute secret commands or read referenced
  secret files. Credentials are resolved from named local environment variables
  only during explicit inventory/apply operations. Keys are never rendered.
- TOML-managed providers are visible but read-only remotely. This first UI supports
  basic provider creation and credential edits; it does not edit arbitrary existing
  provider options, toggle/delete providers, enroll machines interactively, manage
  OAuth login sessions or assign agent loadouts. Edit the registry to add instances;
  advanced provider configuration remains an admin-API operation.
- Master keys remain broad authority. Server credentials/rollback backups remain
  plaintext SQLite; in-flight sockets retain their snapshots. See
  [remote administration](remote-administration.md) for those limitations.
- The shared `byteowlz-tui-kit` is consumed from a pinned, published xlatch-repository
  snapshot because the canonical template workspace contains template placeholders.
  Only the kit crate is linked; no xlatch service or Pi dependency is introduced.

## Terminal program status

The TUI and explicitly applied foreground `fleet` commands emit OSC 7501 revision
0.3 records with `app=eavs`: working for active operations, blocked/question for
forms, blocked/permission for preview confirmation, done/error for settled applies,
and idle for browsing/cancellation. Summaries are fixed, never form values or raw
errors. Only current batch targets determine the operation outcome.

Emission requires the actual terminal streams (all stdin/stdout/stderr for CLI
apply); `EAVS_PROGRAM_STATUS=0` disables it. Redirected output, JSON inventory and
headless gateway traffic do not receive OSC. No feature query is sent: revision 0.3
permits reports without detection and well-behaved terminals ignore unknown OSC.
This is advisory presentation, not authorization or remote service/job health.
OAuth-login emission and terminal/mux/SSH interoperability remain unimplemented or
unverified; the existing gateway/API behavior is unchanged.

Shared language-neutral emitter fixtures/checker live beside `byteowlz-tui-kit` in
the xlatch checkout. Run the complete local emitter/PTY check with an explicit path:

```sh
just test-program-status /path/to/xlatch/crates/byteowlz-tui-kit/conformance/osc7501.py
```

The suite is not yet published/pinned into CI. Ordinary `test-fleet-tui` exercises
workflow/status assertions; providing the shared checker adds independent wire
validation. This is not full consumer/lifetime/mux compliance certification.

Proof: Rust mock/client/keyboard/TestBackend tests plus `just test-fleet-tui` on
POSIX. The latter launches the actual executable in an isolated PTY with synthetic
credentials and a loopback fake admin server; it verifies preview/cancel, one
confirmed mutation, terminal restoration and no secrets in terminal output.
No real fleet was contacted. Windows build/runtime behavior has not been exercised.
