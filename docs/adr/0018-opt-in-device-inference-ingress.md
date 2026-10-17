# Opt-in device inference ingress

Status: implementation decision for the user-approved Mac Studio / real iPhone pairing workstream (`eavs-f3e9`). Not a deployment or acceptance claim.

## Problem

ADR0017's operator listener requires its general listener capability even for redemption and scoped serving. Copying that capability to a phone would expose unrelated operator surfaces. Tailnet membership is not device authorization.

## Decision

Add an explicitly enabled, separate **loopback-only** device listener to the same gateway process and AppState. An operator-managed HTTPS reverse proxy supplies the externally selected origin; no Funnel/public exposure is implied. The existing operator listener, middleware and administration routes remain unchanged. Device ingress has an exhaustive route set:

- POST `/device-enrollment/redeem`: possession of a short-lived, single-use invitation authorizes only atomic scoped-key issuance, through the existing KeyStore transaction. A bounded process-local rolling claim throttle precedes database work. It is ingress abuse protection, not durable quota enforcement.
- GET `/v1/eavs/catalog`: a currently valid enrolled device key is mandatory; reuse the existing key-filtered catalog operation.
- POST `/v1/eavs/device/revoke`: disable only the authenticated enrolled device key. No supplied key ID/hash selects authority. The phone verifies a fresh 401 with an intentionally invalid inference body (which cannot generate even if revocation unexpectedly fails) before claiming revocation; a lost receipt remains an uncertain outcome.
- POST `/:provider/v1/pi/messages`: a currently valid enrolled device key is mandatory; reuse the existing Pi interface and authoritative physical post-policy inference admission. No routing choice, pricing check, accounting restriction or upstream credential resolution is reimplemented here.

There are no administration, configuration, credential export, generic proxy, job, OAuth or filesystem routes on device ingress. Arbitrary operator-created virtual keys are not device enrollment credentials. Errors are static and every response is no-store. Bodies, tokens and keys must not be logged by the ingress/reverse proxy. The listener is disabled by default, cannot bind a non-loopback address, and does not make plaintext HTTP a supported external phone transport.

A QR contains a versioned JSON invitation document with an explicit HTTPS origin, leaf-certificate SHA-256 pin and opaque invitation, not a credential-bearing URL. On the operator's custom Headscale network, a dedicated Caddy instance can terminate TLS using a private machine-local certificate/key, without modifying shared proxy routes or installing a root CA on the phone. The phone explicitly approves the QR's origin/pin, then checks the exact certificate, hostname, validity and TLS policy for every request; the pinned certificate is only a per-connection anchor. This is not a global TLS bypass, blind discovery trust or general system trust-store modification. Certificate rotation requires deliberate repair/re-enrollment. Scanning and confirmation do not submit inference. Redemption does not carry master/general listener/upstream secrets; the returned independent device key belongs in Keychain. The phone subsequently fetches the authoritative scoped catalog before selecting a model. No existing conversation/history is uploaded; each selected eavs connection/model has an independent context. Local/scripted inference modes keep their network-denying configuration. The Swift integration uses PiDurableKit's actual `pi-messages` provider through its public `Models` and URLSessionConfiguration seams, not a second SSE parser or a fabricated zero-usage native answer. The pinned SDK sources remain unchanged. Per-harness transport restricts origin/path, strips its local lease marker, rejects redirects/private error bodies, and requires TLS 1.3. Initial eavs mode exposes no tools, disables automatic retries and opens a fresh independent context on each explicit start rather than replaying interrupted paid work. Tools, if later enabled, must retain phone-owned execution-time authority.

Apple ATS additionally requires a domain exception to permit per-connection trust evaluation with a private certificate. The development phone build declares only the exact `genai-hulk.example.com` host (no subdomains), following Apple's `NSExceptionAllowsInsecureHTTPLoads` documentation. This exception does not itself change HTTPS trust; the eavs transport still rejects HTTP before credential dispatch and requires the pin, hostname, validity and TLS 1.3 checks. There is no global ATS exception. Other private hosts need deliberate provisioning or publicly trusted TLS, not silent trust expansion.

## Boundaries

The existing operator-only enrollment surface remains compatible. Invitation templates are not cascading grants: outstanding invitations follow the original snapshot checks; issued child revocation is explicit. Same-process shared KeyStore/cache ownership avoids adding cross-process cache-coherence claims. Existing capped-Pi-accounting refusals remain in force. Device labels and local profile IDs are not attested device identity.

HTTPS origin trust and reverse-proxy ownership are explicit deployment prerequisites. Do not replace an existing live gateway until its candidate and rollback path are checked. Real acceptance requires the actual signed phone app, selected gateway/model, one explicit real request and actual subsequent revocation denial; synthetic tests only cover regressions.
