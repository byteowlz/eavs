# Harnesses

Which coding agents eavs connects, and how. Config facts below come from each
harness' documentation and another gateway's published notes, and need confirming by a
conformance run. Selection criteria and the plugin
mechanism: [ADR-0024](adr/0024-harness-connections-as-machine-applied-desired-state.md).
A harness counts as supported only with a recorded conformance run (tested
version and date); everything else is untested. Nothing below is implemented
yet except where an `eavs device` command is named.

## Built in

| Harness | Config | Wire API | Notes | Status |
|---|---|---|---|---|
| Claude Code | `~/.claude/settings.json` `env` | Anthropic Messages | settings token wins over local login | `eavs device claude-code`; probed 2.1.296 |
| Pi | `~/.pi/agent/models.json` | per provider | | `eavs device pi`; no recorded conformance run |
| oh-my-pi | `~/.omp/profiles/<name>` | as Pi | profile-scoped | untested |
| Prime Agent | `~/.prime/agent` or `$PRIME_AGENT_CODING_AGENT_DIR` | as Pi | Pi's file format; sends no User-Agent | untested |
| Codex | `~/.codex/config.toml` | Responses | key by `env_key` reference | untested |
| OpenCode | `~/.config/opencode/` | per provider | `{env:VAR}` key references | untested |
| Antigravity CLI (`agy`) | `~/.gemini/antigravity-cli/settings.json` | Gemini | endpoint and key from environment only | untested |
| DeepSeek Harness (`dsh`) | `~/.dsh/profiles/<name>/cordis.patch.yml` | Chat Completions | key in `~/.dsh/.env` and its key store | untested |
| fx | `~/.fx/settings.json` | Chat Completions | sends no User-Agent; identify by key | untested |
| Droid (Factory) | `~/.factory/settings.json` `customModels` | Messages, Responses or Chat Completions per model | takes a literal `apiKey` per model | untested |
| Kimi Code | `~/.kimi-code/config.toml` (2.x), `~/.kimi` (old) | Chat Completions | provider must exist before models and default; reverse order on removal | untested |
| Qwen Code | `~/.qwen/settings.json` `modelProviders` | Chat Completions | key by `envKey` reference into its `env` or `~/.qwen/.env` | untested |
| Hermes Agent | `~/.hermes/config.yaml` `providers` | Chat Completions | | untested |
| Crush | `~/.config/crush/crush.json` | per provider | its picker writes `~/.local/share/crush/crush.json`, which wins | untested |
| Zed | `~/.config/zed/settings.json` | per provider | keys live in the OS credential store, keyed by API URL | untested |
| Cursor CLI | `~/.cursor/cli-config.json` | Cursor's own | model choice only; no custom endpoint known, so it cannot route through eavs. The separate Cursor Private Inference build reads `CURSOR_LOCAL_AGENT_BASE_URL`/`_API_KEY` | to verify |
| Copilot CLI | `~/.copilot/settings.json` | GitHub's own | model and effort choice only; no custom endpoint known, so it cannot route through eavs | to verify |

## Plugins

Data-only descriptors in `$XDG_CONFIG_HOME/eavs/harnesses/<id>.toml`, or an
`eavs-harness-<id>` executable for harnesses data cannot express.

| Harness | Why not built in |
|---|---|
| Gemini CLI | deprecated for personal accounts on 2026-06-18 in favour of Antigravity CLI; still supported for Gemini Code Assist Standard/Enterprise |
| aider, Goose | existing adapters become example plugins |

## Generic

Any tool that reads `OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL` or
`GOOGLE_GEMINI_BASE_URL` plus a key from the environment gets copyable settings.
