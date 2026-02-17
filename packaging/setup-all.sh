#!/usr/bin/env bash
set -euo pipefail

# Complete setup script for releasing eavs to GitHub, Homebrew, and AUR
# Usage: ./setup-all.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "========================================="
echo "EAVS Release Infrastructure Setup"
echo "========================================="
echo ""

# Check prerequisites
echo "Checking prerequisites..."

if ! command -v git &> /dev/null; then
    echo "Error: git is not installed"
    exit 1
fi

if ! command -v gh &> /dev/null; then
    echo "Warning: gh CLI is not installed"
    echo "Install from: https://cli.github.com/"
    echo ""
fi

echo "Prerequisites check passed!"
echo ""

# Homebrew tap setup
echo "========================================="
echo "1. Homebrew Tap"
echo "========================================="
echo ""
echo "The Homebrew tap is already configured at: byteowlz/homebrew-tap"
echo ""
echo "The eavs formula will be automatically updated on releases."
echo ""
echo "Required secret: TAP_GITHUB_TOKEN"
echo "  - Generate a PAT at: https://github.com/settings/tokens"
echo "  - Add 'repo' scope"
echo "  - Add secret at: https://github.com/byteowlz/eavs/settings/secrets/actions"
echo ""

# Check if formula exists
if curl -sf "https://raw.githubusercontent.com/byteowlz/homebrew-tap/main/Formula/eavs.rb" &> /dev/null; then
    echo "Formula exists in tap"
else
    echo "WARNING: Formula not found in tap. Add it manually."
fi
echo ""

# AUR setup
echo "========================================="
echo "2. AUR Package"
echo "========================================="
echo ""

# Check if AUR SSH key exists
if [ -f ~/.ssh/aur ]; then
    echo "AUR SSH key found at ~/.ssh/aur"
else
    echo "AUR SSH key not found. Generate one:"
    echo "  ssh-keygen -t ed25519 -f ~/.ssh/aur -C 'AUR SSH key'"
    echo ""
    echo "Then add to AUR account:"
    echo "  1. Go to https://aur.archlinux.org/account/<username>/edit"
    echo "  2. Paste contents of ~/.ssh/aur.pub"
fi
echo ""

echo "Required secrets for AUR:"
echo "  - AUR_SSH_PRIVATE_KEY: Content of ~/.ssh/aur"
echo "  - AUR_EMAIL: Your email address"
echo "  Add secrets at: https://github.com/byteowlz/eavs/settings/secrets/actions"
echo ""

# Check if package exists in AUR
if curl -sf "https://aur.archlinux.org/cgit/aur.git/plain/PKGBUILD?h=eavs" &> /dev/null; then
    echo "Package exists in AUR"
    echo "  URL: https://aur.archlinux.org/packages/eavs"
else
    echo "Package does not exist in AUR yet."
    echo "Run ./setup-aur.sh to create initial package."
fi
echo ""

# Summary
echo "========================================="
echo "Summary"
echo "========================================="
echo ""
echo "Release workflow is configured!"
echo ""
echo "To do a release:"
echo "  1. Update version: just release X.Y.Z"
echo "  2. Or manually:"
echo "     - Update Cargo.toml version"
echo "     - Commit changes"
echo "     - Tag: git tag vX.Y.Z"
echo "     - Push: git push origin vX.Y.Z"
echo ""
echo "The workflow will:"
echo "  - Build binaries for all platforms"
echo "  - Create GitHub release"
echo "  - Update Homebrew formula"
echo "  - Update AUR package"
echo ""
echo "See docs/RELEASE.md for more details."
echo ""
