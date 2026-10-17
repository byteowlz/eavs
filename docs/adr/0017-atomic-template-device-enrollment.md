# Atomic template-based device enrollment

Status: Accepted bounded implementation per explicit rollout approval; verification
and limits below. This does not implement ADR0001 delegated run grants.

A root/master-authenticated administrator explicitly selects an existing virtual
key as a **scope template**, approves an invitation TTL (1–600 seconds) and an
independent device-key lifetime (1 second–365 days). The template is not a parent
grant. No upstream, master or listener credential is copied. Existing virtual-key
permissions and OAuth owner/account bindings are reused without inventing refresh
credentials. Budget/rate-capped templates (including a budget window alone) fail
closed: cloning their process-local counters would multiply rather than preserve
limits.

## Schema and transaction boundary

`device_enrollment_v1` is additive in the virtual-key SQLite database:

- `token_hash TEXT PRIMARY KEY`: SHA-256 of a 244-random-bit invitation; never raw.
- `template_hash TEXT NOT NULL`: explicit source virtual key, not a cascading FK.
- `authority TEXT NOT NULL`: serialized snapshot of the persisted permissions
  bytes, owner, OAuth user/account and validity timestamps. Original permission
  bytes avoid randomized HashSet reserialization; byte changes fail closed even
  if semantically equivalent. Arbitrary metadata/name/usage are not copied.
- `expires_at INTEGER NOT NULL`: server UTC seconds; equality is expired.
- `device_ttl INTEGER NOT NULL`: administrator-approved device lifetime.
- `consumed INTEGER NOT NULL DEFAULT 0`: consumed by redemption or revocation.

KeyStore owns migration and SQL; no pool/SQL capability is exposed publicly. The
first redemption transaction statement conditionally consumes a live invitation
with UPDATE RETURNING (SQLite write serialization). The same transaction loads
and validates the live template and snapshot, inserts an independent virtual key
with fresh credential/ID, empty usage, allowlisted device provenance, and expiry
at the lesser of approved lifetime and template expiry. Invalid template or INSERT
failure rolls consumption back. After commit, both existing caches are published
synchronously with no intervening await, before the raw key is returned once.
Restart recovers committed keys through the existing key cache loader.

## Authorization and transport

POST `/admin/device-invitations` and `/admin/device-invitations/revoke` require
constant-time configured root/master authorization (not merely listener access).
POST `/device-enrollment/redeem` requires the invitation in a bounded JSON body.
All three remain inside existing listener authentication. Listener capability must
be provisioned independently; it is neither copied into the new key nor returned.
Bodies are limited to 2048 bytes, errors are static, handler responses are
`Cache-Control: no-store`, and tokens/keys never occur in route paths or logs.
No CLI was added: avoid introducing secret argv transport or a second issuance
implementation. HTTP can be consumed using protected request/header files/stdin.

## Consequences and limits

- Disabling/deleting/changing the template blocks outstanding invitations.
  **Template revocation after redemption is not child revocation.** Disable the
  explicit child key through existing administration. This is not delegation.
- OAuth bindings are identifiers only; their upstream secrets stay with the
  gateway. There is no borrowed refresh token, route replication or inference.
- Existing request-time effective virtual-key authority remains authoritative,
  including resolution, provider policy and existing fail-closed operations.
- Response loss or ambiguous commit never returns a credential from an error.
  A committed-but-undelivered child can remain; do not replay to recover its raw
  key. Root administration can identify/disable children by ID/provenance and mint
  a replacement. No plaintext recovery secret is stored.
- There is no invitation auto-cleanup or built-in IP throttling in this bounded
  feature. Root minting and the existing listener boundary constrain exposure;
  production deployments should rate-limit unauthenticated enrollment attempts
  at their authenticated ingress. Never log request/response bodies there.
- Cache publication updates the redeeming KeyStore only. Separate simultaneously
  running gateway processes have the existing cache-coherence limitations; the
  durable atomic single-winner guarantee is SQLite-wide, not an assertion of
  multi-process live authentication/revocation coherence or global budgets.
- This supersedes the earlier ledger-only prototype. The obsolete
  `enrollment_invitations` table is ignored; its receipts cannot mint keys.

## Proof surface

Offline Rust tests under `keys::store::enrollment` and `enrollment::http` cover
independent connections, exactly one winner, immediate authentication through both
caches, reopen recovery, trigger-induced rollback/no ghost child, expiry/replay/
revocation, changed template scope/bindings/validity, rejected caps, exact scope and
bounded child expiry, HTTP root versus listener authorization, body limits and
private no-store errors. These tests do not claim delegated/global enforcement.
