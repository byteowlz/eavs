# Per-instance provider administration and credential rotation

Status: First implementation under verification (`eavs-ysmk`).

Independent eavs instances retain independent configuration, credentials and admin
identity. Existing master-key-authenticated HTTP administration is the initial
control surface; this does not establish a central fleet authority or synchronize
secrets through Git. Pi remains an optional consumer, never a dependency.

## Decisions

- Resolve HTTP, WebSocket, native-job and OAuth provider routes through one
  immutable per-request runtime snapshot. TOML declarations retain precedence;
  enabled admin-managed declarations supply otherwise absent names. Invalid
  stored declarations are unavailable, not silently replaced with another route.
- Remote credential rotation targets admin-managed providers only. TOML collisions
  return a conflict requiring explicit migration, not an invisible shadow update.
- Credential writes require the current metadata revision (`updated_at` rendered
  as RFC3339). SQLite compare-and-swap plus a serialized writer commits the new
  configuration and one previous configuration atomically. Successful rollback
  consumes that backup and publishes another revision. Stale callers get 409.
- Provider administration responses expose names/revisions/enabled state and a
  small type-metadata allowlist, never the stored config, credentials or headers.
  Rotation responses contain only provider name and revision; audit logs contain
  operation metadata, not keys. Rotation accepts literal keys, not secret refs.

## Boundaries

One running gateway process owns each provider database/cache. CAS persists across
restart, but this is not cross-process live cache coherence. In-flight HTTP/WS
requests keep their captured credentials; rotation affects subsequent resolution,
not already-open upstream sockets. Existing job-handle signatures can become
invalid after provider-key rotation; this feature does not migrate them.

The SQLite provider store already holds plaintext credentials. The single rollback
snapshot also contains the previous credential: protect the database directory and
backups, and do not commit them. Encryption/secret-store integration, scoped admin
roles, durable audit history and managed fleet rollout remain follow-ups. The
master key is broad authority; use TLS or an authenticated private tunnel/network,
never cleartext public exposure. Tests use synthetic credentials and mock upstreams.

## Alternatives

Updating TOML remotely or layering silent store overrides makes the active source
ambiguous and creates cross-file recovery problems. Replicating gateway databases
mixes credentials and runtime accounting. Git sync remains declarations and local
secret references only (ADR-0005).
