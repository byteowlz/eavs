# Bounded device enrollment

Implemented as KeyStore-owned transactions in `src/keys/store/enrollment.rs`.

All routes are POST, have 2048-byte body limits and retain existing listener
authentication (`X-Eavs-Token`). Administration additionally needs
`Authorization: Bearer <master>`. Never put invitation or credential values in
URLs, argv, logs or shell history. Use protected request/header files or stdin;
responses contain one-time secrets and must not be logged.

- `/admin/device-invitations`: body
  `{"template_id":"existing-key-id","invitation_ttl_seconds":60,"device_ttl_seconds":3600}`.
  Returns `{"token":"...","expires_at":1234567890}` once. Root chooses the exact
  existing scope template. Rate/budget-capped templates are rejected.
- `/admin/device-invitations/revoke`: body `{"token":"..."}`. Idempotent for
  well-shaped tokens; returns `{"revoked":true}` without revealing existence.
- `/device-enrollment/redeem`: body `{"token":"..."}`. Returns a fresh independent
  `{"key":"...","key_id":"device-...","expires_at":"..."}` once. No root
  credential required, but listener capability remains independently required.

Handler errors are static: `unauthorized`, `invalid_enrollment`, or
`enrollment_unavailable`. Responses use `Cache-Control: no-store`; malformed and
oversized bodies never echo input. The listener middleware retains its existing
unauthorized response. There is no inference, upstream secret transfer, listener
capability replication, new CLI secret-argv path, or desktop modification.

Template changes/revocation invalidate outstanding invitations. After successful
redemption, the new device key is independent: revoke it explicitly using existing
key administration. Response loss cannot recover its plaintext; an administrator
must disable any orphan and explicitly approve a replacement. Never infer a
successful issuance from a network error or retry consumed invitations.
