# Spec: Radius — single dial, proportional scale, sharp-at-0

Shape is governed by **one dial** and a **proportional categorical scale**. The dial is per-tool (oqto = `0`, sharp); the scale is shared.

## The dial

A single root radius value. `radius = 0` means every tier is `0` → dead sharp. Increasing it scales every tier up proportionally.

## The scale (proportional)

The categorical tiers are multiplicative functions of the dial so that **all tiers are `0` at `dial = 0`** and **all tiers are `> 0` at `dial > 0`**:

| tier | value |
|------|-------|
| `--radius-sm` | `calc(dial * 0.5)` |
| `--radius-md` | `calc(dial * 0.75)` |
| `--radius` / `--radius-lg` | `dial` |
| `--radius-xl` | `calc(dial * 1.25)` |

Components pick a tier by depth/role: innermost controls use `sm`, cards use `lg`, full-bleed containers use `xl`. Because the scale is proportional, the innermost (`sm`) rounds as soon as the dial is non-zero — it is never starved to sharp by a large outer padding.

## Why proportional, not additive concentric

Strict concentric geometry requires `outer − inner = inset` (the padding between boxes). Under that rule the inner radius is `max(0, outer − cumulative-inset)`, which decays to zero whenever the dial is smaller than the cumulative inset — so at normal dial values the innermost "never has a radius." That conflicts with the requirement that the innermost rounds when the dial is turned up.

Proportional scaling satisfies both real requirements:
- **sharp-at-0** (oqto's identity): every tier is a multiple of the dial, so all are `0` at `dial = 0`.
- **innermost rounds**: `sm` is `0.5 × dial`, non-zero whenever the dial is non-zero.

Harmonious proportional rounding is how real UIs (Apple, shadcn) nest in practice; strict shared-arc concentricity is a special case, not the default.

## Exact concentric (opt-in)

Components that explicitly want shared arc centers use the helpers:
- `childRadius(parent, inset) = max(0, parent − inset)` — decay inward (clamps at sharp).
- `parentRadius(child, inset) = child + inset` — grow outward (leaf-anchored; never clamps).

These are the strict-concentric tools, available but not the default path.

## Implementation home

The rule is target-agnostic (this file). Each framework impl implements it natively:
- `impls/shadcn-ts/` — emits `--radius` (the dial) + the proportional `--radius-sm/md/lg/xl`; components pick tiers, or call the helpers for exact concentric.
- `impls/iced-rs/`, `impls/gpui-rs/` — later, same rule.

The playground ships a radius slider so the scale is verifiable live.
