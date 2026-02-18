set positional-arguments

default: help

# List available tasks
help:
    just --list

# Format code
fmt:
    cargo fmt

# Check for errors
check:
    cargo check

# Run clippy lints
lint:
    cargo clippy

# Run tests
test:
    cargo test

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
    cargo test --quiet
    cargo clippy --quiet -- -D warnings
    cargo fmt -- --check
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
