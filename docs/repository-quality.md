# Repository quality: staged template adoption

EAVS now has template-aligned lint/check tooling. **The repository is not yet
lint-compliant.** No warning baseline, file exclusions, `--cap-lints`, or CI
`continue-on-error` hides the debt. Tracking: overall **eavs-h0pm**, lint cleanup
**eavs-8nxb**, config/schema generation follow-up **eavs-5006**. The known missing configuration sections now have coverage and an offline validation gate; the schema remains manually maintained.

## Template comparison and deliberate differences

Compared against `../templates/rust-cli` and `../templates/rust-workspace`:

- The single-package manifest uses `[lints.*]`, not workspace inheritance. It
  enables the templates' all/pedantic/nursery/cargo groups, panic/unwrap/expect,
  suppression, debug/TODO, async, process/memory and complexity guardrails.
- **Staged exception:** existing-code lint levels are `warn` in ordinary builds
  and tests, instead of the templates' `deny`. Unsafe code also remains `warn`
  rather than `forbid`: `src/capture.rs` has existing libc calls requiring review.
  This preserves a usable build/test remediation loop, not a passing lint gate.
  `just clippy` and CI use `-D warnings`. Once source debt is addressed, promote
  manifest warnings to deny and reviewed unsafe policy to forbid. Non-ASCII
  identifiers are already forbidden. No blanket `warnings = allow` is used.
- The templates' narrow allowances remain: transitive duplicate crate versions,
  four numeric-cast lints, CLI stdout/stderr, and boolean-heavy CLI structs.
  These are inherited policy decisions, not a whitelist of current violations.
- Complexity thresholds are 15 (cognitive), 200 (types), and 100 (function lines).
  The deprecated `cyclomatic_complexity` lint/config alias is omitted in favor
  of its cognitive-complexity replacement. Domain identifiers supplement, rather
  than replace, Clippy's built-in documentation identifiers.
- Template ast-grep rules are retained, including their reviewed-suppression
  rule. The command uses `ast-grep` (not the ambiguous `sg`) and `--error` so
  warning-severity rules genuinely fail. Tests are not excluded. These patterns
  are supplementary, not an exhaustive substitute for Clippy.
- Checks cover all targets/features and rustdoc. Default tests run without live
  provider requirements; integration-feature code is compile-checked, not run
  against external providers. Tooling tests exercise the copy helper in isolation.
- Edition **2021**, package version **0.8.3**, dependencies, and features are
  unchanged. The templates' 2024 edition migration and JSON/TOML-only dependency
  policy are not smuggled into a lint-only change. Their example-config generator
  is not copied. `just validate-config` uses pinned standard JSON Schema validation
  and a limited mapped-field scanner; it is not generated Rust/serde parity.

## Gates and observed results

Local toolchain: rustc/Clippy 1.98.0; ast-grep 0.42.1. Counts are observations,
not thresholds. CI uses stable Rust and ast-grep 0.42.1. CI has independent jobs
with no tolerated failures, so one failure does not hide the other gates.
`just check-all` is intentionally fail-fast; `just release-check` calls it too.
The existing shared release workflow is unchanged; branch protection/release
policy must enforce Check externally if required.

| Gate | Current result |
| --- | --- |
| `just fmt-check` | PASS after parent-owned formatting cleanup |
| `just check-all-features` | PASS after parent fixed integration `AppConfig` fixture |
| `just clippy` | FAIL: compiler summaries report 1,616 binary and 2,536 test-target errors (overlapping) |
| `just lint-rust-ai-guardrails` | FAIL: 718 findings: 701 unwrap/expect, 17 Clippy suppression attributes, 0 debug/TODO |
| `just doc-check` | PASS after crate/docs markup fixes (previously 11 diagnostics) |
| `just test` | PASS: 464 tests at the release-safety review snapshot |
| `just test-tooling` | PASS: 3 isolated Python tests |
| `just validate-config` | PASS: standard JSON Schema validation, mapped field coverage and 4 regression tests |
| `just check-all` | FAIL at Clippy; not a green baseline |

The Clippy/ast-grep counts above are the initial migration baseline, not a new count after the release-safety fixes. These gates still need a clean run before release; passing runtime tests is not a waiver.

A diagnostic-only run of `cargo clippy --locked --all-targets --all-features
--message-format=json` (without `-D warnings`) collected **4,152 emitted warnings,
2,529 unique diagnostics across 78 lint codes**. Deduplication uses lint code,
message, and primary file/byte spans; targets overlap, and multiple primary spans
can differ by target. Do not add the strict target totals or ast-grep findings to
this count. The diagnostic-only exit success is **not** a passing quality gate.

Largest unique categories:

| Lint | Count |
| --- | ---: |
| unwrap_used | 774 |
| uninlined_format_args | 559 |
| doc_markdown | 200 |
| allow_attributes_without_reason | 88 |
| allow_attributes | 86 |
| unreadable_literal | 79 |
| redundant_closure_for_method_calls | 72 |
| default_trait_access | 59 |
| map_unwrap_or | 47 |
| option_if_let_else | 44 |
| cognitive_complexity | 43 |
| missing_const_for_fn | 36 |
| too_many_lines | 35 |
| panic | 30 |

Initial pre-policy baseline: formatting failed in 8 diff regions across 4 files;
Clippy with default features reported 24 binary warnings and 37 test warnings
(including 24 duplicates). All-features Clippy failed with E0063 in the integration
fixture. Default tests already passed (451). The parent's fixture repair enabled
full test-target lint collection, so the earlier approximately 1,636-error snapshot
was not a complete measure of debt. Parent-owned formatting and fixture edits are
separate from this tooling work.

Detailed local logs: `/tmp/eavs-lint-baseline/` and `/tmp/eavs-lint-after/`, including
`clippy.jsonl`, strict gate logs, and `ast-grep.json`. Temporary clean/violating
fixtures confirmed each ast-grep rule returns success/failure respectively.
Manifest comparison confirmed all non-lint TOML content unchanged. CI itself has
not been executed remotely. Fix debt in reviewed source-focused changes; rerun
all gates before claiming compliance. Do not raise thresholds or add suppression
lists merely to obtain green checks.
