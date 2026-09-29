# Harness adapters over shared provisioning operations

Status: Accepted direction; implementation pending (`eavs-4m7n.3`, `eavs-4m7n.4`).

Use harness-specific inspect/render adapters over a shared plan, validation, application and recovery layer. The CLI remains standalone; Oqto and optional UI integrations consume the same operations. Target broad harness coverage using verified source references, with fx as an early lightweight-agent target and Pi/OpenCode as initial format references.

## Consequences

Advertise capability per operation, not a blanket supported badge. Project configuration can merge global models; isolated config directories or supported launch options may be necessary for workdir-only selection. Adapter inspection must not execute credential commands, follow secret-file references or perform inference.

The shared coordinator preserves unrelated entries and detects concurrent modifications. Cross-file/database application is not inherently atomic: backups, a durable recovery journal and tested compensation are required. The exact transaction implementation remains proposed.

## Alternatives considered

Independent config writers in every UI or adapter duplicate destructive-operation logic. Restricting the model to Pi/OpenCode prevents lightweight harness provisioning from being a first-class use case. Blindly copying a competitor's integrations without fixture verification misrepresents compatibility.

See [gateway scope enforcement](0001-gateway-enforced-loadout-scopes.md).
