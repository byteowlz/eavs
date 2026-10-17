# Fleet administration TUI layout

Primary object: independent eavs instances and their provider inventories. Two
panes, not a generic three-pane browser. Instance selection is independent of
cursor position; the provider pane belongs to the focused instance.

```text
 EAVS / Fleet                                   2 selected • idle
 ╭ Instances ─────────────╮ ╭ Providers · dev-east ──────────────────╮
 │ ✓ dev-east        ready│ │ › anthropic     admin     enabled       │
 │ ✓ dev-west        ready│ │   openai        TOML      read-only     │
 │   laptop       offline │ │                                         │
 │                       │ │ Source and revision shown on selection. │
 │                       │ │ No secrets displayed.                    │
 ╰───────────────────────╯ ╰─────────────────────────────────────────╯
 Space select · Tab focus · a add · c rotate · u rollback · : commands
```

Header/status bars use kit Bar fills; content uses Surface, muted rounded panel
borders and one Accent on focus. Empty/error states teach the next action.
Command palette uses the same action registry as direct keys.

Add/rotation uses a transient form (local environment variable name, never a raw
secret). Enter opens a preview of named target instances and pinned revisions;
Enter there applies, Escape cancels. Batch results remain per-instance. Remote
TOML providers are shown but never silently overwritten. No network mutation
occurs on startup, inventory refresh, form entry or preview.
