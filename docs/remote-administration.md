# Remote administration of independent instances

Each eavs instance has its own provider database and master key. Pi is not a
runtime dependency. Use authenticated TLS or a private tunnel for administration;
do not expose plaintext admin traffic publicly. No fleet daemon or secret sync is
installed automatically.

## Admin-managed providers

`POST /admin/providers` accepts `{name, config, enabled}` with
`Authorization: Bearer <master-key>`. Names must be lowercase letters/digits,
hyphens or underscores. Configuration must deserialize as a supported provider.
Enabled stored declarations now participate in HTTP, WebSocket and native-job
routing without restart. TOML providers retain precedence; attempts to update or
rotate a TOML-managed name return 409. Explicitly migrate configuration before
using remote administration—there is no automatic TOML rewrite.

`GET /admin/providers/:name` returns metadata including `updated_at`, not secret
configuration. Provider response `config` contains only recognized type metadata;
credentials, custom headers and URLs are intentionally omitted. This changes the
old admin response contract, which returned the complete configuration.

## Rotate and roll back

- `POST /admin/providers/:name/credentials`
  accepts `{expected_revision, api_key}`. The revision is the exact `updated_at`
  string from current provider metadata. API keys must be non-empty literal values,
  not `env:`/`keychain:` references. Do not put secrets in command-line arguments
  or shell history; feed JSON through protected stdin using your secret manager.
- Success returns only `{name, revision}`. Subsequent requests use the new key.
- Stale revisions return 409 without modifying the provider.
- `POST /admin/providers/:name/credentials/rollback`
  accepts `{expected_revision}` using the revision returned by rotation. It
  restores the immediately preceding configuration and returns a fresh revision.
  Successful rollback consumes the backup. Further config updates invalidate it.

Updates and their rollback snapshot commit in one SQLite transaction. Concurrent
rotations of the same revision have one winner. One gateway process owns each
provider database; multiple processes sharing a live cache are not supported.
Restart reloads committed credentials and rollback state. In-flight requests and
already-open WebSockets keep their original snapshot. Rotating provider credentials
can invalidate existing signed native job handles.

## Security boundaries

The master key currently grants broad administration; scoped remote-admin roles
and a durable fleet audit trail are not implemented. Logs record rotation name,
revision and rollback operation, not keys. Existing provider storage and the one
rollback snapshot hold plaintext secrets: protect the database and backups and
exclude them from Git sync. This is remote administration, not cross-machine
credential replication or authorization to deploy to an instance.

Verification uses synthetic keys, isolated SQLite stores and mock upstreams:
authentication, invalid/stale updates, live key changes, redacted output,
concurrent compare-and-swap, restart persistence and rollback. No paid probe or
real remote instance was used.
