# Oqto Pi-streaming invitation / QR

`eavs device invite --client oqto --qr` is an **operator issuance command**, not inference or discovery. It creates a short-lived, single-use invitation for an independent scoped device key. The typed descriptor explicitly selects the **Pi `pi-messages` streaming interface**; the server-side template, not the QR hint, determines authorization.

## Preconditions

- Configure the gateway's separate loopback `serve --device-listen` ingress and a dedicated externally reachable HTTPS reverse proxy first (ADR0018). The command does not install/start a service, generate a certificate, change shared proxy routes or probe providers.
- Obtain an existing scoped virtual key's **key ID** for `--template`; this is not a display name. Existing enrollment restrictions and template snapshot/expiry checks apply; rate/budget-capped templates are refused. Issued children are independent: disabling the template is not child revocation.
- Independently verify the public device origin and its **leaf certificate SHA-256**. The CLI does not download/implicitly trust a certificate. The phone checks the actual pin, hostname, validity and TLS policy. Private iOS origins also need deliberately provisioned ATS support or publicly trusted TLS.
- Place root authorization and any separate operator listener capability in existing environment variables. Arguments contain **names**, never credential values. These credentials are sent only to the explicit operator origin; neither enters the descriptor.

## Usage

Create a trusted private parent directory (Unix/macOS; Windows ACL publication is not implemented):

```sh
mkdir -p "$HOME/.local/state/eavs/invitations"
chmod 700 "$HOME/.local/state/eavs/invitations"
```

Preview offline first (replace `KEY_ID`, origin and pin):

```sh
eavs device invite \
  --client oqto --connection-type pi-messages \
  --template KEY_ID \
  --admin-url http://127.0.0.1:3033 --allow-loopback-http \
  --admin-key-env EAVS_MASTER_KEY --listener-key-env EAVS_AUTH_TOKEN \
  --origin https://genai-hulk.example.com:9443 \
  --certificate-sha256 HEX64 \
  --output "$HOME/.local/state/eavs/invitations/iphone-01" \
  --qr --dry-run --json
```

`HEX64` must be the actual 64 hexadecimal characters, not this placeholder. Omit `--listener-key-env` only when the operator listener does not require that capability. The operator URL is distinct from the phone's public device origin. Plain HTTP is allowed **only** for an explicitly approved literal loopback operator URL, never the phone origin.

Remove `--dry-run` to approve interactively; add `--yes` for noninteractive issuance. Default invitation TTL is 300 seconds (maximum 600); requested child TTL defaults to 3600 seconds and is clipped by server authority/expiry. Flags: `--invitation-ttl-seconds`, `--device-ttl-seconds`.

- Dry-run reads no credential environment values, creates no output and makes no HTTP requests. It is validation, **not** a live readiness or authorization proof.
- A new directory is reserved mode `0700`; `invitation.json` and optional `invitation.svg` are created mode `0600`. Existing directories/files are never overwritten. The parent must already exist, be trusted and not group/world writable.
- `--qr` writes SVG, **not** a secret QR to terminal scrollback. Explicitly open that local SVG to scan it, or import the private JSON file into Oqto.
- Human output and `--json` contain only a secret-free receipt: schema version 1, state, client/type, origin/pin, paths, requested TTLs and (after issuance) server `expires_at` Unix seconds. Never a token or root/listener credential. A machine-mode failure emits a schema-versioned error and exits nonzero.
- Operator requests use verified TLS (or the approved loopback exception), no ambient proxy, no redirects, a timeout and no application retry. Private server error bodies are not printed.
- Lost/invalid issuance responses are uncertain; do not automatically issue another invitation. Let the outstanding TTL expire or inspect the operator. If local publication fails after a token is known, the CLI attempts **one** authenticated revocation and reports whether it succeeded. Partial private files may remain for inspection.

## Versioned wire contract

New descriptors are UTF-8 JSON, at most 2048 bytes, with **exactly five fields**:

```json
{
  "version": 2,
  "connection_type": "pi-messages",
  "origin": "https://gateway.example:9443",
  "certificate_sha256": "<64 hexadecimal characters>",
  "token": "<opaque single-use enrollment token>"
}
```

The QR contains the exact JSON bytes, not a URL/query string. There are no provider, model, upstream, root or listener credentials, filesystem references or executable commands. `connection_type` is a client protocol selector, **not an authorization grant**.

Updated Oqto accepts legacy version 1 (the same four fields without `connection_type`, implicitly Pi) and version 2 only when `connection_type` is exactly `pi-messages`. Unknown versions/types, mixed schemas and extra fields fail locally **before claim**. No silent downgrade. Older v1-only Oqto builds need updating before scanning new descriptors.

After explicit trust approval:

1. POST `/device-enrollment/redeem` with body `{ "token": "..." }` over pinned HTTPS. No general listener/master credential is sent by the phone.
2. Store the returned independent device key in Keychain; public recovery profile contains no bearer. An attempted-token digest survives reopening, including across v1/v2 rewrapping.
3. GET `/v1/eavs/catalog` with the scoped device Bearer. Select an authorized supported/priced model; registration starts no inference and imports no prior histories.
4. Only after Send: configure Pi `api: "pi-messages"`, base URL `<origin>/<provider>/v1/pi`; POST `<origin>/<provider>/v1/pi/messages`. Inference/usage/scope checks remain shared gateway authority. No automatic retry/fallback.
5. Explicit own-key revocation: POST `/v1/eavs/device/revoke`. Confirm a fresh 401 using an intentionally invalid Pi `{}` body, which cannot generate even if revocation fails. Forget-only is distinct and does not revoke the server grant.

## Reproducible regressions

```sh
just test-device-invitations
# Optional independent decoder proof, using synthetic artifacts only:
EAVS_INVITE_SYNTHETIC_FIXTURE=/absolute/new/private-fixture just test-device-invitations
rsvg-convert /absolute/new/private-fixture/invitation.svg -o /absolute/new/private-fixture/invitation.png
# On macOS, after explicitly transferring these synthetic files:
swift scripts/test_device_invitation_qr.swift invitation.png invitation.json
```

The macOS Vision decoder must recover **exact JSON bytes**. Do not run artifact capture/logging against real invitations. Oqto consumer tests: `just ios-test-eavs-tls` in `oqto-mobile`; these include typed v2/legacy v1 compatibility and rejection tests, actual Pi runtime and private-TLS regression fixtures. Synthetic proofs do not substitute for signed real-phone QR pairing, the one approved real inference and actual revocation denial. That acceptance remains pending.
