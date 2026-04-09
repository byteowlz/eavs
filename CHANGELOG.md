# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.10] - 2026-02-19

### Added
- `eavs models configured` command to show models from eavs.toml shortlists
- `--discover` flag to probe provider endpoints for available models
- Model discovery for OpenAI-compatible, Google Gemini, and Ollama endpoints
- Non-chat model filtering (removes fine-tunes, embeddings, TTS, image, realtime models)
- JSON output support via `--json` flag
- Optional provider filter argument

## [0.5.3] - 2026-02-17

### Added
- Release infrastructure for automated GitHub releases, Homebrew, and AUR publishing
- Build binaries for Linux x86_64/ARM64 and macOS Intel/Apple Silicon
- Homebrew formula in byteowlz/homebrew-tap with automatic updates
- AUR package with automatic PKGBUILD updates
- Just recipes: `release`, `release-check`, `changelog` for automated release workflow
- Comprehensive release documentation in `docs/RELEASE.md`

### Changed
- Updated README with installation instructions for Homebrew, AUR, Cargo, and pre-built binaries
- Added `test_model` field to provider config for specifying the model used by `eavs setup test` and `eavs setup test-all`
- Added automatic test model discovery from `~/.pi/agent/models.json` (matched by base_url) as a fallback when `test_model` is not set in config
- Test model resolution precedence: config `test_model` > pi models.json > built-in defaults
- Azure AI Foundry configuration now uses standard provider types (openai, anthropic, openai-compatible) instead of a dedicated "foundry" provider type. This gives users more control over API format selection based on the model they're using (GPT models use OpenAI format, Claude models use Anthropic format, etc.)

### Fixed
- Fixed `eavs setup test-all` summary incorrectly counting failed tests as passed when user selects "Continue anyway?" (e.g., showing "14/14 passed" when only 3 actually passed)

### Removed
- Removed dedicated `foundry` provider type. Users should now configure Azure AI Foundry using the appropriate provider type for their model (see config.example.toml for examples)

## [0.5.13] - 2026-02-25

### Added
- Enhanced mock provider with 12 configurable test scenarios: simple_text, tool_call, multi_tool, tool_call_then_text, error_mid_stream, rate_limit, server_error, timeout, connection_reset, thinking, long_text, malformed_sse
- Scenario selection via `X-Mock-Scenario` request header or model name (e.g., `mock/tool-call`)
- Configurable per-chunk streaming delay via `X-Mock-Delay-Ms` header (default: 30ms)
- Mock scenario models registered in `/providers/detail` for models.json generation
- Auto-routing for `mock/*` model names to mock provider
- Full chunk audit logging with timestamps for mock responses
- `/catalog/lookup` endpoint for model metadata from models.dev
- Provider detail endpoint now exposes headers, api_version, and compat settings

## [Unreleased]

## [0.6.1] - 2026-04-09

### Fixed
- Dynamic mock responses now work with array-style message content (OpenAI format with `content: [{type: "text", text: "..."}]`), fixing compatibility with TUI clients

### Added
- Provider templates for quick provider setup (`eavs setup` with templates)
- `eavs secret list --all` to show all stored secrets
- Dynamic mock mode with predefined responses from config (`[mock_responses.<name>]`)

### Changed
- Security: disabled sqlx default features to remove unused mysql/rsa dependencies
