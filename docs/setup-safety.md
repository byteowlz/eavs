# Setup writes and smoke-test defaults

`eavs setup add` now refuses an existing provider name before collecting another
credential or running its test. Saving rechecks under a persistent sibling
`.setup.lock`, so competing setup processes cannot append the same name. Quoted
TOML keys count as the same name. Import/batch mode skips existing names without
replacing credentials; default-provider creation also checks parsed TOML.

All setup mutations parse existing/proposed TOML and use a synced temporary file
plus atomic replacement. Comments and unrelated existing text are preserved.
Invalid/unreadable configs are refused, not silently replaced. Scaffolding only
initializes empty/missing configs. Names and values are TOML-serialized rather
than interpolated; existing Unix permissions/owner/group are preserved or the
write is refused, new files are private, and existing symlinks target the original
file. Linux tests prove two-process single-winner creation and permission/symlink
behavior. Mac/Windows runtime behavior has not been exercised.

Locks coordinate eavs setup writers, not arbitrary editors. A last content check
catches observed external edits; it is not atomic CAS against non-cooperating
writers. Do not delete a lock file while setup runs. Existing inline provider maps
that cannot be safely extended by appending may require an explicit manual edit;
setup refuses invalid output rather than rewriting comments away.

## Recent fallback models

- Anthropic: `claude-haiku-5-5` (hyphens, not `claude-haiku-5.5`).
- OpenAI/Responses: `gpt-5.4-mini`; probes use Responses rather than legacy chat
  `max_tokens` for modern GPT models.
- Google/Vertex: `gemini-3.8-flash`.
- Groq: `openai/gpt-oss-20b`; Cerebras: `gpt-oss-120b`.
- xAI: `grok-4.20-non-reasoning`; OpenRouter: `openai/gpt-5.4-mini`.
- Mistral retains its `mistral-small-latest` alias. Azure deployment-specific and
  custom/local defaults are not silently changed to a guessed hosted model.

These are maintained suggestions, not proof of account/region/deployment access.
Explicit `--model`, configured `test_model`, and existing optional Pi-model
inference precedence are preserved. No automatic paid fallback/retry is added.
IDs were checked in current official [Anthropic](https://platform.claude.com/docs/en/about-claude/models/overview),
[OpenAI](https://developers.openai.com/api/docs/models/gpt-5.4-mini),
[Google](https://ai.google.dev/gemini-api/docs/models),
[Groq](https://console.groq.com/docs/models),
[Cerebras](https://inference-docs.cerebras.ai/models/overview) and
[xAI](https://docs.x.ai/docs/models) documentation; no live inference was run.

## Already-corrupt configs

The new writer will not automatically decide which duplicated credential to keep.
Back up the file privately, retain the intended `[providers.anthropic]` block and
remove the other complete duplicate block (including its nested tables). Then use
`eavs setup test anthropic --model claude-haiku-5-5`. An optional
`test_model = "claude-haiku-5-5"` belongs in the surviving provider block.
The already-installed binary is unchanged until you build/install the fix.
