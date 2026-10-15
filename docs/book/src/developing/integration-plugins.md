# Declarative integration plugins

A service integration package groups an existing remote MCP connection, an event
protocol mapping, and an optional SOP template. Fastmail is the first sample.
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

The host only supports `mcp_mail_poll_v1` / `email_received` in schema version 1.
It polls at the source's configured interval (60 seconds by default); it does not
subscribe to JMAP push or MCP notifications. The manifest supplies the tool,
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
whose URL exactly matches `https://api.fastmail.com/mcp`. Review the manifest
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
```

The loopback smoke test uses synthetic mail and a deterministic SOP. It checks
baseline suppression, plugin pause/resume through the authenticated API, one
completed event run, restart without replay, and rejection of changed package
bytes. It neither accesses real mail nor sends a real notification.
