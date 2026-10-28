# eavs console (native)

Native GPUI console for eavs gateways.
Manage the gateway on this machine and any remote gateway (e.g. the server
that holds all connections) from one window: providers, models and their
availability, and subscription/credit usage.

It is a client of each gateway's operator API, like the web console at
`/admin` and the CLI; it never holds upstream credentials.

```bash
just run       # launch
just check-all # fmt, clippy (warnings denied), drift check, tests
just render    # headless renders with synthetic fixtures -> /tmp/eavs-console-render
just inspect   # read-only check of the local gateway through the client
```

## Gateways

The gateway on this machine is found from `~/.config/eavs/config.toml`
(operator port, listener token, master key). Declare others in
`~/.config/eavs-console/config.toml` (written on first run):

```toml
[[instances]]
name = "server"
url = "http://127.0.0.1:13044"           # ssh -L 13044:127.0.0.1:3044 server, or https:// on the tailnet
token = "keychain:server-listener-token" # eavs secret set server-listener-token
master_key = "keychain:server-master-key"
```

Credentials must be references (`env:NAME`, `keychain:<account>` in the
`eavs` keychain service). Plain HTTP is accepted only to loopback.

## Structure

```
crates/
  eavs-console-core/    # instance registry, operator client, presets, view rules (no gpui)
  eavs-console-design/  # scheme -> roles -> gpui theme set (no gpui); eavs graphite/lime scheme
  eavs-console/         # the app; the only crate that imports gpui-kit
```

The look is the existing eavs scheme (ADR-0008). There is no Design Studio
target for eavs yet; the build is exploratory until one is selected.
