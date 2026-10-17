# Local fleet CLI and TUI over per-instance administration

Status: First implementation under verification (`eavs-9ppy`).

A local fleet registry names independent eavs instances and local environment
references for their admin credentials. Shared client operations support both
CLI and TUI; no mandatory controller/server, secret replication or Pi dependency
is introduced. Instance registration is declarative TOML, not authenticated device
identity.

## Decisions

- Inventory reports active configuration source and revision, including disabled
  stored declarations. TOML declarations retain precedence and are read-only here.
- Explicit instance selection and previews precede mutations. TUI confirmation
  consumes the same pinned plan; CLI `--yes` authorizes a freshly built plan in that
  invocation. Errors remain per-instance and do not trigger retry/fallback.
- Add uses an atomic create-only endpoint; existing-name races cannot silently
  overwrite declarations. Rotation/rollback reuse ADR-0006 revision checks.
- Transport rejects redirects, ambient proxies and URL credentials. HTTPS is
  required except loopback HTTP tunnels; response memory/time are bounded. Error
  content is locally classified rather than echoing even an upstream `code` field.
- ANSI theme, palette/action registry, selection and terminal lifecycle come from
  `byteowlz-tui-kit`. The kit is pinned to a consumable published xlatch snapshot;
  the template workspace itself contains placeholders and is not consumable as a
  Git dependency. This is not an xlatch runtime dependency.

## Boundaries and alternatives

No distributed batch transaction, live provider probe, secret-file execution,
background sync, agent grant or central fleet identity is claimed. Arbitrary
provider editing, interactive machine enrollment and scoped admin roles are later
operations over the same core, not TUI-specific secret/config writers. WebSocket
rotation and plaintext storage limitations remain those of ADR-0006.

A separate per-UI writer or copying gateway databases would duplicate security
and recovery logic. A central mandatory controller would couple otherwise
independent deployments. See [operator guide](../fleet-tui.md).
