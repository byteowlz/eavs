# Graph administration UI studies

Five interactive, synthetic-only prototypes—not a production admin UI:

1. **Control room:** dark graph canvas, resource explorer, scope inspector.
2. **Route lanes:** warm editorial layout with machine → workspace → harness → provider lanes and a horizontal inspector.
3. **Patch bay:** tactile infrastructure rack, source/provider jacks, visible cables.
4. **Tenant atlas:** airy spatial map and floating inspector.
5. **Change desk:** graph alongside a per-instance review workbench.

Open `index.html` through the supplied HTTP server for the comparison gallery. The gallery previews are generated from the actual interfaces, not stock images.

## Interactions

- Switch tenant context and explicitly select the eavs instances to consider. Each tenant has isolated in-memory drafts and targets. Only the first synthetic instance starts selected, not the whole fleet.
- A sample workspace-to-provider intent starts staged to make preview discoverable. Discard it to begin with an empty plan.
- Click a machine, workspace or harness jack, then a provider jack to stage an access intent. Alternatively use the source inspector. Change Desk includes an expandable connection editor.
- Drag cards, fit the layout, undo intent changes, remove individual drafts, or discard the draft.
- Preview pins **sample** per-instance revisions and lists only selected targets. Simulate independent receipts; a second eligible target demonstrates a revision conflict without retries. Local inference is deliberately absent from some sample targets to demonstrate a blocked/skipped outcome.
- Keyboard: Tab to a source jack, Enter, then Tab to a provider jack and Enter. Escape dismisses preview without applying.
- Narrow screens use a scrollable graph and stacked inspector.

## Boundaries

All machines, workspaces, harnesses, provider availability, model lists, revisions, policies and receipts are fixtures. They do not prove protocol compatibility, gateway authorization, connectivity, accounting, health or actual provider/model availability. A harness name or graph edge is never an authorization credential. Effective scope must be enforced after route resolution by the real gateway; run descendants must not widen authority.

No gateway/provider APIs are called, no credentials are imported or displayed, and no actual grants or config are written. Applying only renders **simulated** receipts and retains the draft. Edges represent policy intent, not secret replication. TOML-owned provider configuration stays read-only; policy intent may reference a provider only where that instance has it. Provider edits and credential rotation are separate operations and are not implemented here.

Everything is self-hosted, including Manrope, Instrument Serif, IBM Plex Mono and Barlow Condensed fonts. Their SIL Open Font licenses are in `assets/*-OFL.txt`. Font sources: the matching `ofl/` directories in <https://github.com/google/fonts>. No framework, CDN, telemetry, external assets or runtime package installation is required.

## Host and verify

```sh
python3 mockups/graph-admin/serve.py --port 5280
# Another terminal, with Python Playwright already available:
python3 mockups/graph-admin/test_mockups.py --screenshots
```

`serve.py` binds **127.0.0.1 only**, permits a fixed public asset/page allowlist, denies directory listings and private source files, and accepts no write methods. CSP disallows fetch/XHR/WebSocket connections. Tests cover all five desktop and mobile concepts, target/tenant isolation, all source types, model selection, preview/receipts, keyboard, drag, dedup/undo, asset loading, denied paths/methods and absence of API calls.

The requested tailnet hosting uses a transient user service:

```sh
systemd-run --user --unit=eavs-graph-mockups --property=Restart=on-failure \
  --working-directory="$PWD/mockups/graph-admin" \
  python3 "$PWD/mockups/graph-admin/serve.py"
tailscale serve --bg --http=8789 http://127.0.0.1:5280
tailscale serve status
```

Port 8789 is **tailnet-only**; Funnel is not enabled. Existing 8787/8788 routes are untouched. Use the full tailnet DNS name reported by Serve: the host's short name did not resolve to the serving interface in local verification. Access still depends on the tailnet's ACLs. This transient service is not installed for restart after reboot.

To stop only this prototype, leaving the other routes intact:

```sh
tailscale serve --http=8789 off
systemctl --user stop eavs-graph-mockups.service
```
