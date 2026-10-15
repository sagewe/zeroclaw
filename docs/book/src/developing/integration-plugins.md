# Declarative integration plugins

A service integration package groups an existing remote MCP connection, an event
protocol mapping, and an optional SOP template. Fastmail and Home Assistant are
the included samples.
This is a bounded declarative extension of the native event observer, separate
from executable WASM plugins and their `manifest.toml` protocol.

## Ownership and permissions

- `integration.toml` owns service metadata, the declared MCP endpoint, the mail
  polling mapping, and template text. It contains no credentials or commands.
- `[integration_plugins.instances.<id>]` owns activation, the approved file
  SHA-256, and the reference to an existing MCP server. Instances default disabled.
- `[mcp.servers]` continues to own the URL and encrypted credentials. Agent MCP
  bundles and risk profiles continue to own grants. A package never grants tools.
- `[events.sources.<id>]` owns query and polling cadence; `[events.rules.<id>]`
  owns the target SOP. An integration source refers to the instance instead of
  duplicating its MCP connection.
- The existing event database owns checkpoints and admission receipts. The SOP
  engine owns execution and notification receipts. The dashboard resolves these
  sources on demand rather than storing additional status copies.

The host supports `mcp_mail_poll_v1` / `email_received` metadata mapping in
schema version 1. Fastmail package 0.2.0 additionally declares JMAP EventSource
push. The instance chooses push with `push_enabled = true`; existing instances
keep polling until explicitly migrated. Push opens an authenticated outbound SSE
connection, so no public webhook or inbound port is required. Idle connections
exchange heartbeats without querying mail. On Email state changes, startup, and
reconnect, the existing MCP metadata reader reconciles against the durable mail
checkpoint. Push payloads are wake-up hints and never become agent instructions.
The host checks activation and permissions every two seconds, reconnects after a
90-second heartbeat timeout, and catches up after disconnects without replaying
delivered mail. There is no silent fallback to periodic mail polling. The manifest supplies the tool,
argument names, and JSON paths. The host bounds responses and pagination and
projects only message ID, timestamp, subject, and sender addresses into events.
Body and attachments remain available through the agent's separately granted MCP.

Background polling requires the SOP owner's MCP bundle grant and explicit
`<server>__<poll_tool>` auto-approval. `deny_all_tools`, `excluded_tools`, and
`always_ask` override polling permission. The operator must review the declared
poll tool: a manifest's description is not proof that a remote tool is read-only.

## Install and bind the Fastmail sample

With the daemon stopped, copy
`crates/zeroclaw-config/integrations/fastmail/integration.toml` to
`<install>/shared/integration-plugins/fastmail/integration.toml`. The default
install root is `~/.zeroclaw`. Package directories and manifests must be regular,
non-symlink paths. Discovery is bounded; invalid packages are shown as invalid.
No downloaded code is executed.

After starting the daemon, open **Plugins → Integration plugins** at
`/plugins/integrations`. Bind the package to an existing Fastmail MCP connection
whose URL exactly matches `https://api.fastmail.com/mcp`.
To use push, supply a separate Fastmail **read-only mail JMAP API token** in the
binding editor and select JMAP push. MCP credentials cannot authenticate to JMAP.
The token is encrypted through the existing config secret store; the integration
API returns only whether it is configured. Leaving the field blank preserves the
credential for the same package/connection binding. The authenticated API can
clear it with an explicit empty string while push is disabled. Changing the
package or MCP connection does not silently carry the token to a different binding.

The package pins the JMAP session URL and allowed event origins. Fastmail session
discovery may return a regional event origin; the bundled allowlist includes
`https://api.fastmail.com` and `https://phl.api.fastmail.com`. Other regions require
reviewing and extending the manifest allowlist. Redirects are rejected; discovery
cannot forward credentials to an unlisted origin. See [Fastmail API documentation](https://www.fastmail.com/dev/)
and [JMAP EventSource](https://www.rfc-editor.org/rfc/rfc8620.html#section-7.3). Review the manifest
fingerprint and enable the instance. Configuration can also be written directly
while the daemon is stopped:

```toml
[integration_plugins.instances.fastmail]
enabled = true
package = "fastmail"
manifest_sha256 = "<SHA-256 of the exact integration.toml bytes>"
mcp_server = "fastmail"

[events.sources.fastmail]
enabled = true
kind = "plugin"
plugin = "fastmail"
query = "in:inbox"
poll_interval_secs = 60
```

Grant the connection to the owning agent via its existing MCP bundle, and
explicitly auto-approve `fastmail__search_email` for background reads. Bind a rule
to a SOP with an explicit agent and matching `event_watch` channel trigger. The
sample package previews a `review-mail` SOP template; installation does not write,
replace, or enable an operator SOP. Replace its agent, rule, connection, and
notification-peer placeholders before installing it through the normal SOP flow.
The template scopes review to reading mail and notification to one fixed peer.
Email text is untrusted input, never authority to change rules or destinations.

## Migration, lifecycle, and rollback

For an existing native Fastmail source, keep its ID, query, interval, rule ID,
SOP, and database directory. Replace `kind = "fastmail"` plus `mcp_server` with
`kind = "plugin"` plus `plugin`. The instance owns the old MCP alias. Do not add
another source for the same mailbox.

The bundled sample's equivalent wire mapping retains the legacy checkpoint
revision, preserving the baseline and deduplication window. A changed protocol
mapping or account/connection causes a fresh baseline and supersedes queued work
from the previous revision. Initial baseline collection does not notify old mail.

A plugin switch gates both its event source and its bound MCP server. Saving a
binding schedules the existing supervised daemon reload, rebuilding MCP handles.
Event resolution rechecks live activation, manifest pin, bundle grants, and poll
permission before fetching and before dispatching queued work. Changes do not
undo external actions already started. Editing package bytes without approving
the new digest blocks event resolution and newly assembled MCP grants; existing
agent tool registries are rebuilt on configuration reload. Pause the instance
before editing its package. The page reports invalid pins and supports disabling
an invalid package. Re-enable only after reviewing the new digest.

Back up the configuration, package, binary, web assets, and event database before
migration. To roll back, stop the daemon, restore the old binary/configuration/web
assets, and restart. Do not reset the event database: it contains deduplication
history. Removing an instance restores its MCP server's standalone behavior;
disable the server or remove agent bundle grants when permanently revoking it.

## Verification

```sh
cargo test -p zeroclaw-config integration_plugins --lib
cargo test -p zeroclaw-events --lib
cargo test -p zeroclaw-gateway api_integration_plugins --lib
cargo build --features channel-event-watch,channel-wechat
python3 scripts/ci/event_watch_smoke.py --binary target/debug/zeroclaw --plugin
python3 scripts/ci/event_watch_smoke.py --binary target/debug/zeroclaw --push
```

The loopback smoke test uses synthetic mail and a deterministic SOP. It checks
baseline suppression, plugin pause/resume through the authenticated API, one
completed event run, restart without replay, and rejection of changed package
bytes. It neither accesses real mail nor sends a real notification.

The push fixture additionally verifies no idle mail polling, a prompt state-change
wake-up, reconnect catch-up, persisted enable/disable, and credential redaction.
The mail checkpoint fingerprint depends on the metadata mapping and MCP account,
not the delivery transport or push token, so selecting SSE preserves deduplication.

## Home Assistant package

The `homeassistant` package adds `home_assistant_state_v1` with
`home_assistant_websocket_v1` push. Install its `integration.toml` from
`crates/zeroclaw-config/integrations/homeassistant/` alongside the Fastmail package.
Its MCP selector is the relative path `/api/mcp`: the existing bound server owns
the host, port, TLS choice, and encrypted Authorization header. No address or token
is copied into the package, and no separate push token is required.

The connection derives `/api/websocket` on that same host, authenticates using
the existing bearer credential, and calls `subscribe_entities` with the explicit
entity IDs from the event source. An empty scope is rejected before connecting:
Home Assistant interprets an empty subscription list as all entities. The host
also filters inbound frames to that scope and retains only entity IDs and states,
never attributes, camera data, or unrelated entities. This subscription returns
an ordered initial snapshot and deltas, avoiding the race between separately
reading states and starting a subscription. See the [WebSocket API](https://developers.home-assistant.io/docs/api/websocket/)
and the [official filtered subscription implementation](https://github.com/home-assistant/core/blob/dev/homeassistant/components/websocket_api/commands.py).

The owning agent must have the bound MCP bundle and explicitly auto-approve the
package's read permission, currently `<server>__homeassistant__GetLiveContext`.
Deny-all, exclusions, and approval-required overrides take precedence. This does
not grant appliance control. Other MCP connections, including independently
scoped control helpers, retain their own grants.

```toml
[integration_plugins.instances.home]
enabled = true
package = "homeassistant"
manifest_sha256 = "<SHA-256 of the exact homeassistant integration.toml bytes>"
mcp_server = "home"
push_enabled = true

[events.sources.home]
enabled = true
kind = "plugin"
plugin = "home"
entities = ["light.example"]

[events.rules.home-offline]
enabled = true
source = "home"
sop = "observe-home-state"
states = ["unavailable", "unknown"]
for_secs = 60
cooldown_secs = 300
```

Install the package's optional deterministic `observe-home-state` SOP template,
replacing its agent and rule placeholders. It records conditions without invoking
a model, controlling devices, or sending messages. Notification and action SOPs
remain operator-authored policy. The dashboard derives the correct entity and
condition editors from the package adapter, and shows WebSocket activity instead
of a polling interval.

While connected, only state deltas and application ping/pong messages are used;
there is no REST state polling. A local timer checks pending duration conditions
against the canonical checkpoint. A due condition is evaluated after a successful
application pong, following any state changes already queued by Home Assistant.
On reconnect, pending duration measurement starts again after subscription setup;
time spent disconnected is not evidence of continuity. Already-settled conditions
are not emitted again merely because the stream reconnects. Missing or removed
entities are projected as `unavailable`. Initial setup establishes a baseline.

The package switch and live bundle/read-permission revocation stop observation
and queued admission. Migration from an equivalent legacy Home Assistant source
preserves checkpoint identity. The transport reuses the existing WebSocket and
TLS dependencies. Verify its daemon boundary with:

```sh
python3 scripts/ci/event_watch_smoke.py --binary target/release/zeroclaw --home-push
```

This loopback fixture checks the scoped snapshot, ignored foreign entities, no
REST polling, timed conditions, persisted pause/resume, reconnection and restart
without replay. It does not change a physical device or send a notification.
