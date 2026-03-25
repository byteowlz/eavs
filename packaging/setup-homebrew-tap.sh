#!/usr/bin/env bash
set -euo pipefail

# Script to set up the Homebrew tap repository for eavs
# Usage: ./setup-homebrew-tap.sh

TAP_REPO_DIR="${TAP_REPO_DIR:-$HOME/byteowlz/homebrew-tap}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Setting up Homebrew tap repository..."
echo "Tap directory: $TAP_REPO_DIR"

# Create the tap directory if it doesn't exist
if [ ! -d "$TAP_REPO_DIR" ]; then
    echo "Creating tap directory..."
    mkdir -p "$TAP_REPO_DIR"
    cd "$TAP_REPO_DIR"
    git init
    git remote add origin git@github.com:byteowlz/homebrew-tap.git
else
    echo "Tap directory already exists, updating..."
    cd "$TAP_REPO_DIR"
    git fetch origin
fi

# Create the initial formula template
mkdir -p Formula

cat > Formula/eavs.rb << 'EOF'
# Documentation: https://docs.brew.sh/Formula-Cookbook
#                https://rubydoc.brew.sh/Formula
# PLEASE REMOVE ALL GENERATED COMMENTS BEFORE SUBMITTING YOUR PULL REQUEST!
class Eavs < Formula
  desc "Unified API gateway for LLM providers with virtual API keys and usage tracking"
  homepage "https://github.com/byteowlz/eavs"
  version "0.5.2"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/byteowlz/eavs/releases/download/v0.5.2/eavs-v0.5.2-aarch64-apple-darwin.tar.gz"
      sha256 "TBD_ARM64_SHA256"
    else
      url "https://github.com/byteowlz/eavs/releases/download/v0.5.2/eavs-v0.5.2-x86_64-apple-darwin.tar.gz"
      sha256 "TBD_X86_64_SHA256"
    end
  else
    url "https://github.com/byteowlz/eavs/releases/download/v0.5.2/eavs-v0.5.2-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "TBD_LINUX_SHA256"
  end

  license "MIT"

  def install
    bin.install "eavs"
    (share/"eavs").install "provider-templates.toml"
  end

  test do
    system "#{bin}/eavs", "--help"
  end
end
EOF

# Create the GitHub Actions workflow for the tap
mkdir -p .github/workflows

cat > .github/workflows/update-formula.yml << 'EOF'
name: Update Formula

on:
  repository_dispatch:
    types: [update-formula]

permissions:
  contents: write

jobs:
  update:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Update formula
        env:
          FORMULA: ${{ github.event.client_payload.formula }}
          VERSION: ${{ github.event.client_payload.version }}
          REPO: ${{ github.event.client_payload.repo }}
        run: |
          # Remove 'v' prefix from version
          VERSION="${VERSION#v}"
          echo "Updating formula: $FORMULA to version: $VERSION"
          
          # Download checksums from release
          CHECKSUMS_URL="https://github.com/$REPO/releases/download/v${VERSION}/checksums.txt"
          echo "Downloading checksums from: $CHECKSUMS_URL"
          curl -sL "$CHECKSUMS_URL" -o checksums.txt
          
          # Extract checksums
          ARM64_SHA=$(grep "aarch64-apple-darwin.tar.gz" checksums.txt | cut -d' ' -f1 || echo "")
          X86_64_SHA=$(grep "x86_64-apple-darwin.tar.gz" checksums.txt | cut -d' ' -f1 || echo "")
          LINUX_SHA=$(grep "x86_64-unknown-linux-gnu.tar.gz" checksums.txt | cut -d' ' -f1 || echo "")
          
          echo "ARM64 SHA: $ARM64_SHA"
          echo "X86_64 SHA: $X86_64_SHA"
          echo "Linux SHA: $LINUX_SHA"
          
          # Update the formula
          if [ -f "Formula/$FORMULA.rb" ]; then
            # Update version
            sed -i.bak "s/version \"[^\"]*\"/version \"$VERSION\"/" Formula/$FORMULA.rb
            
            # Update URLs
            if [ -n "$ARM64_SHA" ]; then
              sed -i.bak "s|url \"https://github.com/byteowlz/eavs/releases/download/[^\"/]*/eavs-[^\"-]*-aarch64-apple-darwin.tar.gz\"|url \"https://github.com/byteowlz/eavs/releases/download/v${VERSION}/eavs-v${VERSION}-aarch64-apple-darwin.tar.gz\"|" Formula/$FORMULA.rb
              sed -i.bak "s|sha256 \"[^\"]*\".*ARM64_SHA256.*|sha256 \"$ARM64_SHA\"|" Formula/$FORMULA.rb
            fi
            
            if [ -n "$X86_64_SHA" ]; then
              sed -i.bak "s|url \"https://github.com/byteowlz/eavs/releases/download/[^\"/]*/eavs-[^\"-]*-x86_64-apple-darwin.tar.gz\"|url \"https://github.com/byteowlz/eavs/releases/download/v${VERSION}/eavs-v${VERSION}-x86_64-apple-darwin.tar.gz\"|" Formula/$FORMULA.rb
              sed -i.bak "s|sha256 \"[^\"]*\".*X86_64_SHA256.*|sha256 \"$X86_64_SHA\"|" Formula/$FORMULA.rb
            fi
            
            if [ -n "$LINUX_SHA" ]; then
              sed -i.bak "s|url \"https://github.com/byteowlz/eavs/releases/download/[^\"/]*/eavs-[^\"-]*-x86_64-unknown-linux-gnu.tar.gz\"|url \"https://github.com/byteowlz/eavs/releases/download/v${VERSION}/eavs-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz\"|" Formula/$FORMULA.rb
              sed -i.bak "s|sha256 \"[^\"]*\".*LINUX_SHA256.*|sha256 \"$LINUX_SHA\"|" Formula/$FORMULA.rb
            fi
            
            rm -f Formula/$FORMULA.rb.bak
            
            echo "Formula updated successfully"
            cat Formula/$FORMULA.rb
          else
            echo "Error: Formula file not found"
            exit 1
          fi

      - name: Commit and push
        run: |
          git config user.name "github-actions[bot]"
          git config user.email "github-actions[bot]@users.noreply.github.com"
          git add Formula/${{ github.event.client_payload.formula }}.rb
          git commit -m "Update ${{ github.event.client_payload.formula }} to ${{ github.event.client_payload.version }}"
          git push
EOF

# Create README
cat > README.md << 'EOF'
# byteowlz/homebrew-tap

This is the Homebrew tap for byteowlz projects.

## Installation

```bash
brew tap byteowlz/tap
brew install eavs
```

## Available Formulae

- `eavs` - Unified API gateway for LLM providers with virtual API keys and usage tracking
EOF

# Create .gitignore
cat > .gitignore << 'EOF'
*.gem
*.rbc
/.config
/coverage/
/InstalledFiles
/pkg/
/spec/reports/
/spec/examples.txt
/test/tmp/
/test/version_tmp/
/tmp/
.DS_Store
EOF

echo ""
echo "Homebrew tap setup complete!"
echo ""
echo "Next steps:"
echo "1. Create the GitHub repository: git@github.com:byteowlz/homebrew-tap.git"
echo "2. Push the initial setup:"
echo "   cd $TAP_REPO_DIR"
echo "   git add ."
echo "   git commit -m 'Initial setup'"
echo "   git push -u origin main"
echo "3. Add TAP_GITHUB_TOKEN secret to your eavs repository"
echo "   - Go to https://github.com/byteowlz/eavs/settings/secrets/actions"
echo "   - Add TAP_GITHUB_TOKEN with a PAT that has repo scope for byteowlz/homebrew-tap"
echo ""
echo "The tap is now ready to receive updates from your release workflow!"
