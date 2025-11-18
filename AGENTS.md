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
