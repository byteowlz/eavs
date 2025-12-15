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

# Install binary locally
install:
    cargo install --path .

# Clean build artifacts
clean:
    cargo clean
