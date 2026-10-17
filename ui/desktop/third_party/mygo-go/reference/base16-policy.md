# Spec: base16 support policy

base16 schemes (16 slots) are supported. base24 schemes (24 slots) are first-class. The 8 extra base24 slots (`base10`–`base17`) do not exist in a base16 scheme, so a policy is required. **Neither option below is "the tinted spec's way"** — the tinted spec defines slots and purpose only, never a derivation algorithm; base24 scheme authors hand-pick the extra 8.

## Evidence

From the tinted-theming/schemes repo (2026-06-18): **320 base16 schemes, 185 base24 schemes, only 43 base16 schemes have a same-name base24 twin** (13%). 277 base16 schemes are base16-only. A policy is unavoidable.

## Three-tier policy

### Tier 1 — Twin exists → load base24

If a base24 scheme with the same name exists, load it. Author-tuned, no invention. (43 schemes.)

### Tier 2 — Base16-only → derive the 8 missing

Derive `base10`–`base17` mechanically from the base16 palette:

- `base10` (darker bg) ← darken `base00` by a fixed step
- `base11` (darkest bg) ← darken `base00` by a larger step
- `base12`–`base17` (brights) ← lighten `base08`–`base0E` respectively

Rationale: this is mechanically what a human port does when tinted authors create a base24 scheme by hand. The alternative — aliasing the missing slots to existing ones (duplication) — collapses structural surfaces that depend on the base24-only slots (e.g. a sidebar bound to `base11` would alias to `base00` and vanish into the background).

### Tier 3 — Opt-in duplication (escape hatch)

A scheme may declare `pureBase16: true` to opt into aliasing instead of derivation, for authors who insist "render my 16 colors, invent nothing." Premature to build until a real scheme needs it.

## Accepted consequence

Derived (tier 2) base16 schemes render flatter than a real base24 scheme — the `base10`/`base11` darkening may compress when `base00` is already near-black, and the bright derivations are less reliable than a human eye. This is honest rendering of a smaller palette. If a specific popular base16-only scheme renders poorly, the fix is a per-scheme override, not a different policy.
