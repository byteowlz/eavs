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
    git tag "v$VERSION"
    git push origin main
    git push origin "v$VERSION"
    echo "Release v$VERSION pushed! Workflow will start automatically."

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
