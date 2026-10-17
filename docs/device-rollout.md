# Device and desktop rollout

This is a delivery map, not a claim that mesh routing or release readiness exists.

## Implemented and synthetically verified

1. **Additional Pi native streaming:** shared inference admission, bounded native events, signed-history translation, cache estimates and cancellation; actual five-format Pi decoder/replay proof. See [Pi messages](pi-messages.md), `eavs-zt6g`. Not full live-provider conformance or durable settlement.
2. **Explicit device connections:** `connect`/`enroll`, scoped `status`, preview/CAS `sync`, separate listener capability and environment references. Sync publishes sanitized inventory, **not serving provider configuration**. See [device connections](device-connections.md), ADR0013, `eavs-bz6f`.
3. **Scoped Pi handoff:** `device pi-models` read-only projection and explicit-target `device pi` preview/dual-pin CAS publication, preserving unrelated configuration. Optional shared TypeScript provider library/extension registers the live authorized catalog at startup and `/eavs-refresh`, with no subprocess or models.json mutation. Actual Pi schema/interpolation, Rust/TS parity, Node transport/lifecycle and real Rust/Go workflow tests pass. See [Pi configuration](pi-device-configuration.md), ADR0015, `eavs-p9jr`. The legacy merge corruption bug `eavs-r833` is also source-fixed.
4. **One-time enrollment backend:** root-approved existing uncapped scope template; invitation issue/revoke and atomic redemption into an independent scoped device key. Both authentication caches publish after commit. Six offline database/HTTP tests cover concurrency, rollback, binding changes and private errors. See [enrollment contract](../src/enrollment/README.md), ADR0017, `eavs-s8ke`. This is an HTTP backend, not a finished invitation GUI or credential-vault workflow.
5. **Portable declaration foundation:** `device portable inspect/preview/apply/bind/observe/plan`, secret-free JSON, separate machine-local environment-reference binding, dual-pin CAS, explicit selected routes and bounded credential-free HEAD observations with expiry. Seven regressions and actual CLI smoke pass. See [portable declarations](portable-device-declarations.md), ADR0016, `eavs-gam7`. These are declaration/transport plans, **not live serving configuration, route authorization or automatic failover**.
6. **Native MyGo Connection Desk:** read-only status plus functional **Connect device / Connect Pi** preview/confirmation forms over the Rust operations. Native race tests, vet and build pass using isolated pinned Go1.27.1/MyGo0.3.2 caches, without a global install. Actual X11 software-rendered window and keyboard-opened form were captured; real compiled Rust CLI workflow tests pass. See [desktop](../ui/desktop/README.md), ADR0014, `eavs-wnsh`.

## Remaining work

- Invitation redemption, explicit machine-local secret persistence, issuance/rotation/revocation UX. Current native connection form consumes an operator-provisioned environment reference; tailnet membership never mints authority.
- Reviewed materialization of portable declarations into effective native provider/loadout configuration and optional explicit Git distribution. Inventory must not become configuration implicitly. Never distribute credentials, sessions or spend ledgers.
- Connectivity-aware **live** local/remote eligibility and explicitly authorized alternative selection. HEAD reachability is not provider health, quota, authorization or proof of VPN state. Interrupted/ambiguous inference or tools must not be replayed automatically; no implicit paid fallback.
- Native tray/menu lifecycle, packaging/signing, hardware GPU, Mac/Windows execution and OS screen-reader/accessibility proof. Software X11 evidence is not platform certification.
- Enrolled-peer forwarding only after identity/attenuation/accounting protocol review. No full mesh, transitive trust or offline global-budget claim.

Independently configured public providers can work with internet while Tailscale is down. Shared declarations and connectivity plans do not make server-owned credentials available on another device. Disconnected devices cannot receive immediate revocations; global caps require authority/preallocated allowances or refusal, not independent counters.

## Verification and release boundary

Latest combined Rust run: **582 passed**, and all-target/all-feature check passes. The previously reproduced TPM check/consume race (`eavs-zdqg`) is **not fixed** merely because this run passed. Fresh strict gates remain red: Clippy reports 411 binary / 699 test-target errors, guardrails 212 diagnostics, and rustdoc 5 unresolved links. No diagnostics in the new device/portable/enrollment modules were reported by Clippy. Formatting passes. No release, installed upgrade or service restart is implied. Evidence lives in tracker records, not a green-release claim.

Claude SDK session-adapter work remains parked (`eavs-eb71`, proposed ADR0012). No login, SDK inference or subscription entitlement claim is included.
