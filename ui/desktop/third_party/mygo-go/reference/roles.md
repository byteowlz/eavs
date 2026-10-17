# Spec: Roles

The semantic layer tools consume. A role is an *intent* bound to a slot (e.g. `primary` → `base0B`, `success` → `base0B`, `danger` → `base08`). Roles are the portable surface: a scheme that fills the slots reskins every compliant tool identically at the role layer.

## Membership rule

The closed set contains **only** roles whose intent is universal across screen UIs (web, Electron, iced, gpui) — not echoes of any one framework's token names. Evidence drove the current shape: a real consumer (oqto) uses ~2 foreground intensity levels and ~3 surface depths in practice; the base16 ramp's deeper levels stay available as slots for tools/frameworks that want them, but the portable role layer exposes only levels real UIs actually distinguish.

## The abstract role set — CLOSED (F1: minimal ramps)

### Foreground ramp (2 levels — default + dim)

| role | intent | slot |
|------|--------|------|
| `foreground` | default text | `base05` |
| `muted-foreground` | dimmed / secondary text | `base04` |

Two levels, not five: real UIs distinguish "default" and "dimmed"; the base16 ramp's base03/base06/base07 stay available as slots for tools that need more, but are not portable roles.

### Surface depth (3 levels — base / elevated / sunken)

| role | intent | slot |
|------|--------|------|
| `background` | app background | `base00` |
| `surface` | elevated surface (cards, popovers, panels) | `base01` |
| `surface-sunken` | recessed surface (sidebars, terminals, code) | `base11` |

One `surface` role replaces the shadcn-specific `card`/`popover` split — those are the same abstract intent ("elevated surface") that shadcn happens to name twice at the framework layer. `surface-sunken` exposes the darker base24-only level that was previously reachable only through shadcn's `sidebar` vocabulary, making "put the sidebar darker" a portable concept instead of a framework trick.

### Action & status (8)

| role | intent | slot |
|------|--------|------|
| `primary` | brand / primary action | `base0B` |
| `secondary` | secondary action | `base02` |
| `accent` | accent surface (selections, highlights) | `base02` |
| `success` | positive status | `base0B` |
| `warning` | cautionary status | `base0A` |
| `danger` | destructive / error | `base08` |
| `info` | informational status | `base0D` |

(`secondary` and `accent` both → `base02` but differ in intent — secondary is an action tier, accent is a highlight surface. Kept separate because the intent differs, even when the hue coincides.)

### Structure (3)

| role | intent | slot |
|------|--------|------|
| `border` | borders, dividers | `base01` |
| `ring` | focus ring | `base0B` |
| `input` | input surface | `base01` |

## Total: 16 closed roles

`foreground`, `muted-foreground`, `background`, `surface`, `surface-sunken`, `primary`, `secondary`, `accent`, `success`, `warning`, `danger`, `info`, `border`, `ring`, `input` (15) — plus `muted` retained as a surface-intent alias of `accent` for shadcn compatibility (see below).

**Wait — recount.** The set above is 15. To keep shadcn's `--muted` (the most-consumed surface token in oqto: 28 uses) portable rather than framework-only, `muted` is included as a distinct role = a muted/neutral surface (slot `base02`), distinct from `accent` (a highlight surface, also `base02`). That makes **16**.

Final closed set (16): `background`, `surface`, `surface-sunken`, `foreground`, `muted-foreground`, `primary`, `secondary`, `muted`, `accent`, `success`, `warning`, `danger`, `info`, `border`, `ring`, `input`.

Multiple roles may bind the same slot (e.g. `primary` and `success` both → `base0B`; `secondary`/`muted`/`accent` all → `base02`). That is correct: hue and role are separate indirections. A scheme that swaps the green slot changes every role bound to it; a tool that re-points `success` → a different slot decouples them.

## Per-tool / per-framework vocabulary (NON-portable)

Extensions drawing from the same slots. Examples:

- **Framework (shadcn):** the `*-foreground` pairs (`card`/`card-foreground`, `popover`/`popover-foreground`, `primary`/`primary-foreground`, etc.) and the `card`/`popover` split of `surface`. Defined in `impls/shadcn-ts/`, not here.
- **Tool (oqto):** `terminal-bg`, `terminal-fg`, `code-bg`, `code-fg`, `chart-1..5`, `panel`. oqto-local, never portable.

Vocabulary is structured and intentional (it has a contract), not ad-hoc.

## Alpha is derived, not slotted

`bg-primary/20`, scrims, hovers, and translucent fills are computed from the role's resolved color at the implementation layer. They are never extra slots and never extra roles. This keeps the chromatic base at 24 and makes alpha behave consistently across schemes.
