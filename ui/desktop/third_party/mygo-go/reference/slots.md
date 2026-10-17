# Spec: Slots

The chromatic base. 24 positions for base24, 16 for base16. This is the only layer a *scheme* fills; everything above derives from these.

## base24 — 24 slots

Background ramp (dark → light in dark schemes):

| slot | purpose |
|------|---------|
| `base00` | Default background |
| `base01` | Lighter background (code panels, cards) |
| `base02` | Selection background |
| `base03` | Comments / dim foreground |
| `base04` | Dark foreground (muted text) |
| `base05` | Default foreground |
| `base06` | Light foreground |
| `base07` | Lightest foreground |

Accents (8):

| slot | purpose |
|------|---------|
| `base08` | Red |
| `base09` | Orange |
| `base0A` | Yellow |
| `base0B` | Green |
| `base0C` | Cyan |
| `base0D` | Blue |
| `base0E` | Magenta |
| `base0F` | Brown |

base24 additions (`base10`–`base17`, the +8):

| slot | purpose |
|------|---------|
| `base10` | Darker background |
| `base11` | Darkest background (sidebar, terminal) |
| `base12` | Bright red |
| `base13` | Bright yellow |
| `base14` | Bright green |
| `base15` | Bright cyan |
| `base16` | Bright blue |
| `base17` | Bright magenta |

## base16 — 16 slots

Exactly `base00`–`base0F` above. The `base10`–`base17` slots do not exist; see `base16-policy.md`.

## The chromatic base never expands

24 is the ceiling. No base32, no `base18+`. If a tool needs more hues, gradients, or a brand color outside the palette, that is per-tool vocabulary layered above — never new slots. Alpha/translucency is *derived* from slot values (see `roles.md`), not added as slots.

## Source of slots

The slot definitions follow the tinted-theming specification (purpose and numbering). base24 schemes are hand-authored in that ecosystem; see `base16-policy.md` for how base16-only schemes are handled.
