# Packaging Scripts

This directory contains scripts for setting up and managing releases for eavs.

## Scripts

### `setup-all.sh`
Complete setup script that checks all release infrastructure prerequisites.

```bash
./packaging/setup-all.sh
```

What it does:
- Checks if git and gh CLI are installed
- Verifies Homebrew tap configuration
- Checks AUR SSH key setup
- Displays required secrets and setup steps

### `setup-homebrew-tap.sh` (Reference)
Script to set up the Homebrew tap repository (for reference only, tap already exists at `byteowlz/homebrew-tap`).

### `setup-aur.sh`
Script to set up the AUR package.

```bash
./packaging/setup-aur.sh
```

What it does:
- Clones or initializes AUR package repository
- Generates initial PKGBUILD
- Generates .SRCINFO
- Provides setup instructions

## Quick Start

1. Run the setup check:
   ```bash
   ./packaging/setup-all.sh
   ```

2. Set up Homebrew token:
   - Generate PAT at https://github.com/settings/tokens
   - Add `TAP_GITHUB_TOKEN` secret to eavs repo

3. Set up AUR:
   - Generate SSH key: `ssh-keygen -t ed25519 -f ~/.ssh/aur`
   - Add public key to AUR account
   - Run: `./packaging/setup-aur.sh`
   - Add `AUR_SSH_PRIVATE_KEY` and `AUR_EMAIL` secrets to eavs repo

## Doing a Release

Using just (recommended):
```bash
just release-check    # Verify everything is ready
just release 0.5.3    # Bump version, tag, and push
```

Manual release:
```bash
# Update version in Cargo.toml
vim Cargo.toml

# Commit and tag
git add Cargo.toml
git commit -m "chore: bump version to 0.5.3"
git tag v0.5.3
git push origin main
git push origin v0.5.3
```

## Documentation

See [docs/RELEASE.md](../docs/RELEASE.md) for comprehensive release documentation.

## Release Artifacts

Each release includes:

### GitHub Releases
- Binaries for: Linux x86_64, Linux ARM64, macOS Intel, macOS Apple Silicon
- SHA256 checksums file
- Auto-generated release notes

### Homebrew
- Formula in `byteowlz/homebrew-tap/Formula/eavs.rb`
- Automatically updated via GitHub Actions

### AUR
- PKGBUILD at https://aur.archlinux.org/packages/eavs
- Automatically updated via GitHub Actions

## Support

For issues or questions, see [docs/RELEASE.md](../docs/RELEASE.md) or open an issue on GitHub.
