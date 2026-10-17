# Native adapter validation

Host: macOS arm64, Go 1.27.1 automatic toolchain; MyGo v0.3.2.
Baseline/source pins are in PROVENANCE.md.

Observed `just check`: format clean, go vet, race tests and real Go build pass.
Every copied Studio scheme (oqto/lumen/moss, light/dark) and Base16 Nord matches
all 24 concrete Studio sRGB slots and 16 closed role outputs exactly.
Native vocabulary fields and control radius are asserted. Radius and alias
tests, bounded/malformed Omarchy and source digests pass.

Ripwire quality-delta at baseline 0968afc89 + changes: gating=0. New-code
complexity flags: Omarchy palette validation and two test tables; Go test
entrypoints are incorrectly reported as dead by the name-based reachability
lens. The table branches are validation/assertions, not duplicated hue logic.
Change/contract check records Resolve as a new public symbol; no existing
adapter source/spec values were changed. Compiler/runtime tests remain the
authority for Go type/data correctness, not the graph.

Provenance archives use .txt extensions. This avoids treating archived
TypeScript/Python source as extra runtime implementations or accidental
executables; the role/data/hash tests still read their exact bytes.

Template integration opens real native ui.View windows and supplies independent
render/AX proof; those artifacts are not an adapter-wide native/platform
certification. Linux/Windows native render, high-contrast/wide-P3 behavior and
hardware are unverified. No spec or Studio values were edited.
