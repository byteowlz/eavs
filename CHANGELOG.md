# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Azure AI Foundry configuration now uses standard provider types (openai, anthropic, openai-compatible) instead of a dedicated "foundry" provider type. This gives users more control over API format selection based on the model they're using (GPT models use OpenAI format, Claude models use Anthropic format, etc.)

### Removed

- Removed dedicated `foundry` provider type. Users should now configure Azure AI Foundry using the appropriate provider type for their model (see config.example.toml for examples)
