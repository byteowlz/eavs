# eavs

An inference proxy written in Rust. It sits between clients and configured
model providers, handling upstream credentials, request routing and logging.

Eavs supports several provider protocols, including OpenAI Chat Completions
and Responses, Anthropic and Gemini, plus selected speech, image and other
native endpoints. Support varies by provider and operation; it is not a
universal API adapter.

## Install

Download a binary from [GitHub Releases](https://github.com/byteowlz/eavs/releases),
or build from source:

```bash
git clone https://github.com/byteowlz/eavs.git
cd eavs
cargo build --release
```

The binary is `target/release/eavs`. The examples below assume it is on `PATH`.

## Start locally

Create `eavs.toml`:

```toml
[server]
host = "127.0.0.1"
port = 3033

[providers.default]
type = "openai"
api_key = "env:OPENAI_API_KEY"
```

Set `OPENAI_API_KEY` in your environment, then start the listener:

```bash
export EAVS_AUTH_TOKEN="$(openssl rand -hex 32)"
eavs serve
```

From another terminal with the same listener token:

```bash
curl http://127.0.0.1:3033/health \
  -H "X-Eavs-Token: $EAVS_AUTH_TOKEN"
```

Send requests to `/v1/chat/completions` or the appropriate provider endpoint.
Inference requests may incur upstream charges. The listener token grants access
to eavs; it is separate from the upstream API key. See the
[usage reference](docs/usage.md) for virtual keys and provider selection.

Configuration can also live at `~/.config/eavs/config.toml` (or under
`XDG_CONFIG_HOME`). See the [example configuration](config/config.example.toml)
for options. The schema is manually maintained and [checked against fixtures](docs/repository-quality.md).

Provider-store changes resolve live. The current source also supports explicit
[provider hot reload](docs/provider-reload.md) for TOML declarations via an
administrator preview/apply API, without restarting accepted streams. Automatic
watching and a dedicated CLI adapter are not included; existing installed binaries
need an authorized upgrade before using it.

## Scope and limitations

- Streaming HTTP and selected WebSocket routes, provider configuration, virtual
  keys, request policies and optional logging are available.
- OAuth login/account binding exists, but provider-specific compatibility needs
  verification. It does not imply permission to share subscription access.
- Native job and rerank accounting is incomplete. USD-budgeted or TPM-limited
  keys are rejected on those submission/inference paths; uncapped requests
  may incur upstream charges not recorded in eavs's cost ledger.
- Eavs only controls traffic that reaches it. It is not an agent sandbox;
  network isolation and protection of client credentials remain separate concerns.
- Harness adoption, enforced run loadouts, mesh routing and Git configuration
  sync are planned work, not available features.

## Documentation

- [Configuration, CLI and API reference](docs/usage.md)
- [Remote instance administration and credential rotation](docs/remote-administration.md)
- [Scoped running-instance serving catalog](docs/serving-catalog.md) — authenticated, versioned chat candidates
- [Configure Pi from a connected gateway](docs/pi-device-configuration.md) — safe preview/CAS file handoff or optional dynamic startup discovery
- [Additional Pi messages interface](docs/pi-messages.md) — experimental native streaming with bounded thinking/signature/cache translation; native APIs unchanged
- [Oqto Pi-streaming invitation / QR](docs/device-invitations.md) — explicit operator issuance, private files and typed protocol selection; no pairing-time inference
- [Explicit device connections](docs/device-connections.md) — scoped metadata observation and preview/CAS inventory sync, not automatic routing or credential replication
- [One-time enrollment API](src/enrollment/README.md) — root-approved invitations and atomic independent scoped-key issuance
- [Portable device declarations](docs/portable-device-declarations.md) — local binding and connectivity plans, not automatic serving-route installation
- [Device/desktop rollout and remaining milestones](docs/device-rollout.md) — distinguishes inventory sync from declaration distribution and automatic inference routing
- [Fleet CLI and TUI: setup, previews and keyboard controls](docs/fleet-tui.md)
- [Domain glossary](CONTEXT.md) and [architecture decisions](docs/adr/README.md)
- [Provisioning plan](docs/scoped-provisioning.md)
- [Release process](docs/RELEASE.md)

For command syntax, use `eavs --help` or a subcommand's `--help`.

## Development

```bash
just             # List development commands
just test        # Offline Rust tests
just check-all   # Formatting, compilation, linting, docs and tests
```

Strict quality gates currently report existing lint debt; see
[repository quality status](docs/repository-quality.md). Work is tracked in
`trx`. Read [AGENTS.md](AGENTS.md) before contributing.

## Credits and license

Model metadata comes from [models.dev](https://models.dev/). Some provider
handling was informed by [pi-mono](https://github.com/badlogic/pi-mono).

Licensed under the [MIT License](LICENSE).
