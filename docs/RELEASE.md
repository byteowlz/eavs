# Release Guide

This guide covers how to release eavs binaries to GitHub, Homebrew, and AUR.

## Overview

The release process is automated via GitHub Actions:

1. **GitHub Releases**: Automatically builds binaries for multiple platforms and creates a release
2. **Homebrew**: Updates the formula in `byteowlz/homebrew-tap` via repository dispatch
3. **AUR**: Updates the PKGBUILD in the AUR repository

## Setup Requirements

### 1. Homebrew Tap Setup

The Homebrew tap is already set up at `byteowlz/homebrew-tap`.

**Add TAP_GITHUB_TOKEN secret to eavs repo:**

1. Generate a Personal Access Token (PAT) with `repo` scope:
   - Go to https://github.com/settings/tokens
   - Click "Generate new token" → "Generate new token (classic)"
   - Enable `repo` scope
   - Generate and copy the token

2. Add the secret to the eavs repository:
   - Go to https://github.com/byteowlz/eavs/settings/secrets/actions
   - Click "New repository secret"
   - Name: `TAP_GITHUB_TOKEN`
   - Value: Your PAT
   - Click "Add secret"

**Initial formula setup:**

The eavs formula has been added to `byteowlz/homebrew-tap/Formula/eavs.rb`. It will be automatically updated on releases.

### 2. AUR Setup

**Generate SSH key for AUR:**

```bash
ssh-keygen -t ed25519 -f ~/.ssh/aur -C 'AUR SSH key'
```

**Add SSH key to AUR account:**

1. Go to https://aur.archlinux.org/account/<username>/edit
2. Paste contents of `~/.ssh/aur.pub`
3. Save

**Test SSH connection:**

```bash
ssh -i ~/.ssh/aur aur@aur.archlinux.org
```

You should see a welcome message.

**Add AUR secrets to eavs repo:**

1. Get your private key:
   ```bash
   cat ~/.ssh/aur
   ```

2. Add secrets to the eavs repository:
   - Go to https://github.com/byteowlz/eavs/settings/secrets/actions
   - Click "New repository secret"
   - Name: `AUR_SSH_PRIVATE_KEY`
   - Value: Content of `~/.ssh/aur` (the private key)
   - Click "Add secret"

3. Add email:
   - Name: `AUR_EMAIL`
   - Value: Your email address
   - Click "Add secret"

**Initial AUR package:**

Run the setup script:

```bash
./packaging/setup-aur.sh
```

Then, if the package doesn't exist in AUR yet:

```bash
cd aur-eavs
git add .
git commit -m 'Initial import of eavs'
git push -u origin main
```

Note: You'll need an AUR account to do this.

## Release Process

### Automated Release (Recommended)

1. Update the version in `Cargo.toml`:
   ```toml
   [package]
   name = "eavs"
   version = "0.5.3"  # Update this
   ```

2. Commit the change:
   ```bash
   git add Cargo.toml
   git commit -m "chore: bump version to 0.5.3"
   ```

3. Tag the release:
   ```bash
   git tag v0.5.3
   git push origin v0.5.3
   ```

4. The GitHub Actions workflow will automatically:
   - Build binaries for all platforms
   - Create a GitHub release with checksums
   - Update the Homebrew formula
   - Update the AUR package

### Manual Release Trigger

If you need to re-release or trigger manually:

1. Go to https://github.com/byteowlz/eavs/actions
2. Click "Release" workflow
3. Click "Run workflow"
4. Select branch and enter the tag (e.g., `v0.5.3`)
5. Click "Run workflow"

## Built Platforms

The release workflow builds binaries for:

- `x86_64-unknown-linux-gnu` - Linux x86_64
- `aarch64-unknown-linux-gnu` - Linux ARM64
- `x86_64-apple-darwin` - macOS Intel
- `aarch64-apple-darwin` - macOS Apple Silicon

## Release Artifacts

Each release includes:

- `eavs-<version>-<target>.tar.gz` - Platform-specific binary
- `checksums.txt` - SHA256 checksums for all binaries

## Verification

### Verify GitHub Release

1. Go to https://github.com/byteowlz/eavs/releases
2. Download the binary for your platform
3. Verify checksum:
   ```bash
   sha256sum eavs-*.tar.gz  # Compare with checksums.txt
   ```

### Verify Homebrew

```bash
brew tap byteowlz/tap
brew install eavs
eavs --version
```

### Verify AUR

```bash
# Using yay (recommended)
yay -S eavs

# Using paru
paru -S eavs

# Using makepkg (manual)
git clone https://aur.archlinux.org/eavs.git
cd eavs
makepkg -si
```

## Troubleshooting

### Homebrew Update Failed

Check the workflow run logs:
- Go to https://github.com/byteowlz/eavs/actions
- Find the "publish-homebrew" job
- Check for errors

Common issues:
- `TAP_GITHUB_TOKEN` missing or invalid
- Formula syntax error
- Checksums not found in release

### AUR Update Failed

Check the workflow run logs:
- Go to https://github.com/byteowlz/eavs/actions
- Find the "publish-aur" job
- Check for errors

Common issues:
- `AUR_SSH_PRIVATE_KEY` missing or invalid
- SSH connection failed
- Package already exists with conflicting changes

### Cross-compilation Failed

The `aarch64-unknown-linux-gnu` build requires cross-compilation tools. If it fails:
- Check the "Build aarch64-unknown-linux-gnu" job logs
- Verify the `ubuntu-latest` runner has `gcc-aarch64-linux-gnu` installed

## Release Checklist

Before releasing:

- [ ] Version updated in `Cargo.toml`
- [ ] CHANGELOG.md updated with changes
- [ ] Tests passing: `cargo test`
- [ ] Clippy clean: `cargo clippy`
- [ ] All secrets configured in repository
- [ ] Tagged and pushed: `git tag vX.Y.Z && git push origin vX.Y.Z`

After release:

- [ ] Verify GitHub release created
- [ ] Verify Homebrew formula updated
- [ ] Verify AUR package updated
- [ ] Test install from all sources
- [ ] Announce release (if needed)

## Using the Setup Scripts

### Homebrew Tap Setup

The `packaging/setup-homebrew-tap.sh` script is for reference only since the tap already exists at `byteowlz/homebrew-tap`.

### AUR Setup

```bash
./packaging/setup-aur.sh
```

This creates a local `aur-eavs` directory with:
- `PKGBUILD` - Package build file
- `.SRCINFO` - Package metadata
- Git repository initialized for AUR

## Additional Resources

- [GitHub Actions Documentation](https://docs.github.com/en/actions)
- [Homebrew Formula Cookbook](https://docs.brew.sh/Formula-Cookbook)
- [Arch User Repository Guidelines](https://wiki.archlinux.org/title/Arch_User_Repository)
- [cargo-release Documentation](https://github.com/crate-ci/cargo-release)
