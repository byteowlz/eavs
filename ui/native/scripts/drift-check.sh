#!/usr/bin/env bash
# Verify that documented commands and versioned claims match the real
# manifests. Exit non-zero on any discrepancy. Run in `just check-all`.

set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
err() { echo "drift: ERROR: $*" >&2; fail=1; }

# Required commands exist.
for cmd in cargo just; do
    command -v "$cmd" >/dev/null 2>&1 || err "required command not found: $cmd"
done

# Workspace must resolve (this also fails loudly on a broken manifest).
cargo metadata --no-deps --format-version 1 >/dev/null 2>&1 || err "workspace metadata failed to parse"

# Config/machine output must be JSON/TOML only -- never YAML.
if find . -type f \( -name '*.yaml' -o -name '*.yml' \) -not -path './.git/*' -not -path './.github/*' | grep -q .; then
    err "found YAML outside .github; config/machine output must be JSON/TOML only"
fi

# Exactly one crate may depend on gpui-kit (the app). The design mechanism
# crate must stay free of gpui.
gpui_consumers=$(grep -rl 'gpui-kit' --include=Cargo.toml crates 2>/dev/null)
case "$(printf '%s\n' "$gpui_consumers" | sed '/^$/d' | wc -l | tr -d ' ')" in
    1) : ;;
    *) err "expected exactly one gpui-kit consumer, found: $(printf '%s\n' "$gpui_consumers" | tr '\n' ' ')" ;;
esac

# The design mechanism crate must not import gpui-kit.
if grep -rl 'gpui-kit' crates/*-design/Cargo.toml 2>/dev/null | grep -q .; then
    err "the design system crate must not depend on gpui-kit"
fi

if [ "$fail" -ne 0 ]; then
    echo "drift-check failed."
    exit 1
fi
echo "drift-check: ok"