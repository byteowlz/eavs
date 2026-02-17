#!/usr/bin/env bash
set -euo pipefail

# Script to set up AUR package for eavs
# Usage: ./setup-aur.sh

echo "Setting up AUR package for eavs..."
echo ""

# Check if git config is set
if ! git config user.name >/dev/null || ! git config user.email >/dev/null; then
    echo "Error: Git user.name and user.email must be configured"
    echo "Run:"
    echo "  git config --global user.name 'Your Name'"
    echo "  git config --global user.email 'your@email.com'"
    exit 1
fi

# Clone the AUR package if it doesn't exist
if [ ! -d "aur-eavs" ]; then
    echo "Cloning eavs from AUR (this may fail if package doesn't exist yet)..."
    if git clone ssh://aur@aur.archlinux.org/eavs.git aur-eavs 2>/dev/null; then
        echo "Package already exists in AUR"
    else
        echo "Package doesn't exist in AUR yet, creating local directory..."
        mkdir -p aur-eavs
        cd aur-eavs
        git init
        git remote add origin ssh://aur@aur.archlinux.org/eavs.git
        cd ..
    fi
else
    echo "AUR directory already exists"
fi

# Generate initial PKGBUILD
cat > aur-eavs/PKGBUILD << 'EOF'
# Maintainer: byteowlz <dev@byteowlz.com>
pkgname=eavs
pkgver=0.5.2
pkgrel=1
pkgdesc="Unified API gateway for LLM providers with virtual API keys and usage tracking"
arch=('x86_64' 'aarch64')
url="https://github.com/byteowlz/eavs"
license=('MIT')
depends=('gcc-libs')
source_x86_64=("$pkgname-$pkgver.tar.gz::https://github.com/byteowlz/eavs/releases/download/v$pkgver/eavs-v$pkgver-x86_64-unknown-linux-gnu.tar.gz")
source_aarch64=("$pkgname-$pkgver.tar.gz::https://github.com/byteowlz/eavs/releases/download/v$pkgver/eavs-v$pkgver-aarch64-unknown-linux-gnu.tar.gz")
sha256sums_x86_64=('TBD')
sha256sums_aarch64=('TBD')

package() {
    install -Dm755 eavs "$pkgdir/usr/bin/eavs"
}
EOF

# Generate .SRCINFO
cat > aur-eavs/.SRCINFO << 'EOF'
pkgbase = eavs
	pkgdesc = Unified API gateway for LLM providers with virtual API keys and usage tracking
	pkgver = 0.5.2
	pkgrel = 1
	url = https://github.com/byteowlz/eavs
	arch = x86_64
	arch = aarch64
	license = MIT
	depends = gcc-libs
	source_x86_64 = eavs-0.5.2.tar.gz::https://github.com/byteowlz/eavs/releases/download/v0.5.2/eavs-v0.5.2-x86_64-unknown-linux-gnu.tar.gz
	sha256sums_x86_64 = TBD
	source_aarch64 = eavs-0.5.2.tar.gz::https://github.com/byteowlz/eavs/releases/download/v0.5.2/eavs-v0.5.2-aarch64-unknown-linux-gnu.tar.gz
	sha256sums_aarch64 = TBD

pkgname = eavs
EOF

echo ""
echo "AUR package setup complete!"
echo ""
echo "Directory: aur-eavs/"
echo ""
echo "Next steps:"
echo "1. Generate SSH key for AUR (if you haven't already):"
echo "   ssh-keygen -t ed25519 -f ~/.ssh/aur -C 'AUR SSH key'"
echo ""
echo "2. Add the SSH key to your AUR account:"
echo "   - Go to https://aur.archlinux.org/account/<username>/edit"
echo "   - Paste contents of ~/.ssh/aur.pub"
echo ""
echo "3. Test SSH connection:"
echo "   ssh -i ~/.ssh/aur aur@aur.archlinux.org"
echo ""
echo "4. Add AUR_SSH_PRIVATE_KEY and AUR_EMAIL secrets to your eavs repository:"
echo "   - Go to https://github.com/byteowlz/eavs/settings/secrets/actions"
echo "   - Add AUR_SSH_PRIVATE_KEY with the content of ~/.ssh/aur"
echo "   - Add AUR_EMAIL with your email address"
echo ""
echo "5. Push the initial package (only if it doesn't exist in AUR yet):"
echo "   cd aur-eavs"
echo "   git add ."
echo "   git commit -m 'Initial import of eavs'"
echo "   git push -u origin main"
echo ""
echo "Note: The release workflow will automatically update this package on future releases."
