# Git-backed configuration sync, not runtime-state replication

Status: Accepted direction; merge/apply implementation pending (`eavs-4m7n.6`).

Use an optional Git repository for versioned shared declarations, loadouts and device profiles. Keep upstream credentials, virtual-key values, usage ledgers, reservations and active sessions out of synchronization. Standalone harness configuration sync must work without routing that harness through eavs.

## Consequences

Secrets are machine-local references. Device profiles are configuration, not device identity. Conflicts stop application; no force-push, silent last-writer-wins or changes to unrelated dirty files. Validate revisions before applying and retain a last-working configuration with recoverable writes. Automatic pull/push requires explicit setup, not an installation side effect.

## Alternatives considered

Replicating the entire eavs data directory mixes secrets and accounting with declarative intent. A mandatory sync daemon or central service would undermine standalone and ordinary Git workflows. Manual file copying remains possible but lacks versioned conflict handling.

See [shared operations](0003-harness-adapters-and-shared-operations.md).
