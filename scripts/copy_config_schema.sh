#!/usr/bin/env bash
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_SCHEMA="$SRC_DIR/config/config.schema.json"
DEST_DIR="$SRC_DIR/../schemas/eavs"
DEST_SCHEMA="$DEST_DIR/eavs.config.schema.json"

if [ ! -f "$SRC_SCHEMA" ]; then
  echo "Source schema not found: $SRC_SCHEMA" >&2
  exit 1
fi

# Copy only: publishing must be an explicit, reviewed operation. Never stage
# unrelated changes or implicitly commit/push the neighbouring repository.
if [[ -L "$DEST_DIR" || -L "$DEST_SCHEMA" ]]; then
  echo "Refusing symlink destination: $DEST_SCHEMA" >&2
  exit 1
fi
if [[ -e "$SRC_DIR/../schemas/.git" ]] &&
  [[ -n "$(git -C "$SRC_DIR/../schemas" status --porcelain)" ]]; then
  echo "Schema repository has local changes; review them before copying." >&2
  exit 1
fi
mkdir -p "$DEST_DIR"
cp "$SRC_SCHEMA" "$DEST_SCHEMA"
echo "Copied schema to $DEST_SCHEMA (not committed or published)"
echo "Review the schema diff before explicitly committing and pushing."
