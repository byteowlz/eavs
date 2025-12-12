Awesome CLIs (adhere to these rules when applicable):

Design & UX
• Prefer subcommands for verbs (tool add, tool list) and keep each one focused.
• Be composable: read from stdin, write to stdout, errors to stderr, use clear exit codes.
• Ship sensible defaults; require as few flags as possible.
• Destructive ops: provide --dry-run, prompt only on TTY, support --yes/--force.
• Errors that teach: show cause + fix, suggest near-miss flags/values.

Help & Discoverability
• Fast -h/--help with one-liners; rich help <subcmd> with examples.
• --version prints semver; note deprecations.
• Offer completions generator (tool completions bash|zsh|fish|powershell) and man page/README snippet.

I/O & Formats
• Human output by default; machine output via --json/--yaml (stable schema).
• Respect NO_COLOR/FORCE_COLOR; auto-disable color when not a TTY.
• Verbosity controls: -q/--quiet, stackable -v, --debug, --trace.
• Progress bars/spinners only on TTY; provide --no-progress.

Config & Environment
• Clear precedence: flags > env > config file.
• Use XDG dirs on Unix; sensible Windows paths; --config PATH.
• config init, config show (effective config with sources).

Performance & Reliability
• Fast startup; lazy-load heavy parts.
• Timeouts & retries where networked; --parallel N, --timeout.
• Idempotent behavior; --keep-going and --fail-fast.

Security & Privacy
• Never print secrets by default; mask in logs (--redact).
• Support secrets via env/secret stores; avoid writing to shell history (suggest ENV_VAR=… tool …).

Packaging & Distribution
• Cross-platform builds; reproducible releases; signed artifacts.
• Easy install paths (Homebrew/Scoop/AUR/pkg managers) or single static binary.
• Optional self-update and gentle version-checks (opt-out env).

Observability & Testing
• Logs to stderr with timestamps on --debug/--trace.
• --profile to show timing breakdowns.
• Golden tests for text output; schema tests for JSON; fuzz your flag parser.

Accessibility & Internationalization
• Avoid ASCII art; provide --no-emoji.
• High-contrast, monochrome-friendly output.
• English fallback; locale-aware messages (if you localize).

Standard Flag Kit (copy these across subcommands)

-h, --help · -V, --version · -q, --quiet · -v (stackable) · --debug · --trace · --json/--yaml · --no-color (and honor NO_COLOR) · --dry-run · -y, --yes/--force · --config PATH · --no-progress · --timeout SEC · --parallel N

Bonus power moves
• Context-aware help (suggest flags based on partial input).
• Interactive wizards (init, login) gated behind TTY; pure flags otherwise.
• Plugin system: discover commands from $PATH like tool-foo → tool foo.
• Generate shell completions dynamically from your parser so help & completion never drift.
## Issue Tracking with bd (beads)

**IMPORTANT**: This project uses **bd (beads)** for ALL issue tracking. Do NOT use markdown TODOs, task lists, or other tracking methods.

### Why bd?

- Dependency-aware: Track blockers and relationships between issues
- Git-friendly: Auto-syncs to JSONL for version control
- Agent-optimized: JSON output, ready work detection, discovered-from links
- Prevents duplicate tracking systems and confusion

### Quick Start

**Check for ready work:**
```bash
bd ready --json
```

**Create new issues:**
```bash
bd create "Issue title" -t bug|feature|task -p 0-4 --json
bd create "Issue title" -p 1 --deps discovered-from:bd-123 --json
bd create "Subtask" --parent <epic-id> --json  # Hierarchical subtask (gets ID like epic-id.1)
```

**Claim and update:**
```bash
bd update bd-42 --status in_progress --json
bd update bd-42 --priority 1 --json
```

**Complete work:**
```bash
bd close bd-42 --reason "Completed" --json
```

### Issue Types

- `bug` - Something broken
- `feature` - New functionality
- `task` - Work item (tests, docs, refactoring)
- `epic` - Large feature with subtasks
- `chore` - Maintenance (dependencies, tooling)

### Priorities

- `0` - Critical (security, data loss, broken builds)
- `1` - High (major features, important bugs)
- `2` - Medium (default, nice-to-have)
- `3` - Low (polish, optimization)
- `4` - Backlog (future ideas)

### Workflow for AI Agents

1. **Check ready work**: `bd ready` shows unblocked issues
2. **Claim your task**: `bd update <id> --status in_progress`
3. **Work on it**: Implement, test, document
4. **Discover new work?** Create linked issue:
   - `bd create "Found bug" -p 1 --deps discovered-from:<parent-id>`
5. **Complete**: `bd close <id> --reason "Done"`
6. **Commit together**: Always commit the `.beads/issues.jsonl` file together with the code changes so issue state stays in sync with code state

### Auto-Sync

bd automatically syncs with git:
- Exports to `.beads/issues.jsonl` after changes (5s debounce)
- Imports from JSONL when newer (e.g., after `git pull`)
- No manual export/import needed!

### GitHub Copilot Integration

If using GitHub Copilot, also create `.github/copilot-instructions.md` for automatic instruction loading.
Run `bd onboard` to get the content, or see step 2 of the onboard instructions.

### MCP Server (Recommended)

If using Claude or MCP-compatible clients, install the beads MCP server:

```bash
pip install beads-mcp
```

Add to MCP config (e.g., `~/.config/claude/config.json`):
```json
{
  "beads": {
    "command": "beads-mcp",
    "args": []
  }
}
```

Then use `mcp__beads__*` functions instead of CLI commands.

### Managing AI-Generated Planning Documents

AI assistants often create planning and design documents during development:
- PLAN.md, IMPLEMENTATION.md, ARCHITECTURE.md
- DESIGN.md, CODEBASE_SUMMARY.md, INTEGRATION_PLAN.md
- TESTING_GUIDE.md, TECHNICAL_DESIGN.md, and similar files

**Best Practice: Use a dedicated directory for these ephemeral files**

**Recommended approach:**
- Create a `history/` directory in the project root
- Store ALL AI-generated planning/design docs in `history/`
- Keep the repository root clean and focused on permanent project files
- Only access `history/` when explicitly asked to review past planning

**Example .gitignore entry (optional):**
```
# AI planning documents (ephemeral)
history/
```

**Benefits:**
- Clean repository root
- Clear separation between ephemeral and permanent documentation
- Easy to exclude from version control if desired
- Preserves planning history for archeological research
- Reduces noise when browsing the project

### CLI Help

Run `bd <command> --help` to see all available flags for any command.
For example: `bd create --help` shows `--parent`, `--deps`, `--assignee`, etc.

### Important Rules

- Use bd for ALL task tracking
- Always use `--json` flag for programmatic use
- Link discovered work with `discovered-from` dependencies
- Check `bd ready` before asking "what should I work on?"
- Store AI planning docs in `history/` directory
- Run `bd <cmd> --help` to discover available flags
- Do NOT create markdown TODO lists
- Do NOT use external issue trackers
- Do NOT duplicate tracking systems
- Do NOT clutter repo root with planning documents

For more details, see README.md and QUICKSTART.md.
