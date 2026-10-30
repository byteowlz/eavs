set positional-arguments

default: help

# List available tasks
help:
    just --list

# Format code
fmt:
    cargo fmt --all

# Check formatting without rewriting sources
fmt-check:
    cargo fmt --all -- --check

# Check for errors
check:
    cargo check --locked --all-targets

# Strict lint gate; known source debt fails rather than being suppressed
clippy:
    cargo clippy --locked --all-targets --all-features -- -D warnings

# Alias for the strict lint gate
lint: clippy

# Scan all Rust sources (including tests); warning rules become failing errors
lint-rust-ai-guardrails:
    ast-grep scan --config .ast-grep/sgconfig.yml --error

# Run default tests; integration tests require a live provider
test:
    cargo test --locked

# Isolated ordinary-key native inference ingress; synthetic transports only.
test-inference-ingress:
    cargo test --locked inference_ingress:: -- --test-threads=1

# Offline browser preflight/authentication and SSE transport regressions.
test-cors:
    cargo test --locked cors:: -- --test-threads=1

# Offline file-provider reload and generation-safety regressions.
test-routing:
    cargo test --locked routing::

# Synthetic loopback-only device enrollment/catalog/storage regressions.
test-device:
    cargo test --locked device::

# Dedicated phone ingress: authority separation, revocation and bounded claims.
# Typed invitation CLI/private QR publication; synthetic operator only
test-device-invitations:
    cargo test --locked device::invitation::tests

test-device-ingress:
    cargo test --locked device_ingress::

# Offline scoped serving-catalog authorization and projection regressions.
test-serving-catalog:
    cargo test --locked api::serving_catalog::

# Additional Pi wire interface; offline conversion/admission fixtures.
test-pi-messages:
    cargo test --locked pi_messages::

# POSIX-only offline fleet TUI lifecycle and confirmation proof
test-fleet-tui:
    cargo build --locked
    python3 scripts/test_fleet_tui.py

# Shared emitter fixtures + actual TUI capture validation (explicit local checker)
test-program-status checker:
    python3 -m unittest discover -s "$(dirname {{quote(checker)}})" -v
    BYTEOWLZ_OSC7501_CHECKER={{quote(checker)}} just test-fleet-tui

# Reusable, shared-design-system UI; no package install or gateway calls
build-control-room:
    bun run ui/control-room/build.js

# Pure planning/client/adapter tests against the built shared module
test-control-room: build-control-room
    bun test ui/control-room/tests

# Public declaration contract; explicit existing compiler, never auto-installed
check-control-room-types compiler:
    bun {{quote(compiler)}} -p ui/control-room/tsconfig.json

# Browser/lifecycle proof against an explicitly running read-only demo host
test-control-room-browser base="http://127.0.0.1:5281":
    python3 ui/control-room/tests/browser.py --base {{quote(base)}}

# Offline Pi config merge preservation/fail-closed regression tests.
test-pi-adapter:
    bun test adapters/pi/adapter.test.ts

# Scoped shared provider library and dynamic Pi extension, synthetic loopback only.
test-pi-device-library:
    bun test adapters/pi/provider-library.test.ts adapters/pi/extension.test.ts
    node --test scripts/test_pi_live_node.ts

# Strict source + REAL installed Pi SDK declarations; no install or library waiver.
check-pi-extension-types compiler pi_package:
    node scripts/check_pi_extension_types.mjs {{quote(compiler)}} {{quote(pi_package)}}

# Actual Pi parser/interpolation against an explicit synthetic generated models.json.
test-pi-device-sdk pi_source fixture:
    bun scripts/test_pi_config_sdk.ts {{quote(pi_source)}} {{quote(fixture)}}

# Explicit integration-feature compile gate (no live provider needed)
check-all-features:
    cargo check --locked --all-targets --all-features

# Strict rustdoc gate for the manifest's rustdoc lint group
doc-check:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features

# Isolated standard-library tests for the config/schema copy helper
test-tooling:
    uv run scripts/test_copy_config_schema.py

# Validate manual schema, example config and mapped field coverage
validate-config:
    uv run scripts/check_config_schema.py
    uv run scripts/test_config_schema.py

# Complete local gate; no baseline/continue-on-error exceptions
check-all: fmt-check check-all-features clippy lint-rust-ai-guardrails doc-check test test-tooling validate-config

# Build release binary
build:
    cargo build --release

# Run the proxy server; pass additional flags after `--`
run *args:
    cargo run -- {{args}}

# Install binary and adapters locally
install:
    cargo install --path .
    @mkdir -p "${XDG_DATA_HOME:-$HOME/.local/share}/eavs/adapters"
    @cp -r adapters/* "${XDG_DATA_HOME:-$HOME/.local/share}/eavs/adapters/"
    @echo "Adapters installed to ${XDG_DATA_HOME:-$HOME/.local/share}/eavs/adapters/"
    @cp config/provider-templates.toml "${XDG_DATA_HOME:-$HOME/.local/share}/eavs/provider-templates.toml"
    @echo "Provider templates installed to ${XDG_DATA_HOME:-$HOME/.local/share}/eavs/provider-templates.toml"

# Clean build artifacts
clean:
    cargo clean

# Release: bump version, commit, tag, and push
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{version}}"
    if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "Error: Version must be in format X.Y.Z"
        exit 1
    fi
    echo "Bumping version to $VERSION"
    cargo set-version "$VERSION"
    git add Cargo.toml
    git commit -m "chore: bump version to $VERSION"
    git tag -a "v$VERSION" -m "Release v$VERSION"
    git push origin main "v$VERSION"
    # The public repository receives the snapshot and the tag; the tag push
    # starts the release workflow there.
    byt publish export --tag "v$VERSION" --push
    echo "Release v$VERSION published; the release workflow starts on the tag."

# Check release readiness
release-check:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Checking release readiness..."
    just check-all
    echo "All checks passed!"

# Update CHANGELOG for release
changelog version:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{version}}"
    DATE=$(date +%Y-%m-%d)
    echo "## [$VERSION] - $DATE" > /tmp/changelog_new.md
    echo "" >> /tmp/changelog_new.md
    echo "### Added" >> /tmp/changelog_new.md
    echo "" >> /tmp/changelog_new.md
    echo "### Changed" >> /tmp/changelog_new.md
    echo "" >> /tmp/changelog_new.md
    echo "### Fixed" >> /tmp/changelog_new.md
    cat CHANGELOG.md >> /tmp/changelog_new.md
    mv /tmp/changelog_new.md CHANGELOG.md
    echo "CHANGELOG.md updated for $VERSION"

# Native GPUI console (separate workspace under ui/native, ADR-0022)
console:
    cd ui/native && just run

# Native console gate: fmt, clippy, drift check, tests
console-check:
    cd ui/native && just check-all

