# Events

The Events console separates source configuration, trigger rules, and delivery
history. Native event workers live in `zeroclaw-events` and are owned by the
binary's daemon generation. They do not require an agent channel binding. The
existing channel adapter remains available for legacy `channels.event_watch`
configuration. Build with `channel-event-watch` to include the native workers.

## Ownership

- `events.sources` owns connection references and observation scope. Credentials
  remain in the existing MCP connection; the UI never returns them.
- `events.rules` owns enablement, condition filters and the target SOP. A rule
  references one source. A source may be referenced by several rules; this
  version polls separately for each enabled rule.
- `<data_dir>/events/<rule>/events.db` owns the rule's baseline, deduplication,
  pending delivery, last successful check and delivery receipt. Separate rules
  never share a checkpoint. Reads of this store never perform crash recovery.
- SOP owns semantic decisions, execution, tool permissions and notification.
  Event receipts retain the admitted run ID, not a duplicate run status. The
  console resolves available execution results from the SOP engine on demand.

Fastmail and Home Assistant use polling in this version. SSE, WebSocket source
subscriptions and general webhook ingestion are not implemented. The UI labels
polling explicitly, distinguishes waiting/failed/stale checks from listening,
and does not infer connectivity merely from an enabled configuration.

## Configure

```toml
[events.sources.mailbox]
enabled = true
kind = "fastmail"
mcp_server = "fastmail"
query = "in:inbox"
poll_interval_secs = 60

[events.rules.mail]
enabled = true
source = "mailbox"
sop = "review-mail"
```

The SOP must have an enabled owning agent, with the source's MCP server granted
through that agent's MCP bundles. Existing SOP scopes, untrusted-input screening,
approvals and autonomous action boundaries remain authoritative. No rule can
select its target SOP from event payload text.

For compatibility with existing procedures, the SOP declares a channel trigger
with `channel = "event_watch"` and `alias = "mail"` (the rule ID). This is an
internal ingress contract, not a requirement for a chat-channel binding.

Home Assistant sources use `kind = "home_assistant"` and an explicit `entities`
list. Their rules specify either `states` or numeric `above`/`below` bounds, plus
`for_secs` and `cooldown_secs`. Empty entity lists and wildcards are rejected.

The authenticated `/api/event-monitor` endpoint reads the operator view. PUT uses the
same serialized, authorized configuration commit path as Config, and checks an
optimistic revision so stale browser forms cannot overwrite newer event policy.
Membership changes take effect live. A policy replacement cancels and joins its
old worker before a new worker can open the same checkpoint. Scope changes
establish a new baseline and supersede deliveries from the previous policy.

`POST /api/event-monitor/simulate` evaluates the configured state/duration condition
without reading a source, starting a SOP, writing history or sending messages.
It does not simulate baseline, deduplication, cooldown, or semantic model judgment.

## History

Mail admission and ID retention use the same five-minute cursor window. Search
pages may still contain older messages: after their IDs leave the retained set,
the timestamp cutoff prevents those messages from being emitted again. The
cutoff is taken from the previous checkpoint, before processing the batch, so
catch-up can admit previously unobserved mail spanning a longer outage.

History records candidate events that reach the durable outbox. Suppressed
startup baselines, duplicate messages and ordinary device samples are not
recorded individually. SOP classification can subsequently mark a candidate as
ignored in the UI. Records quarantined during duplicate-queue repair are shown
as `deduplicated`, without claiming that those individual records ran a SOP.
At most 200 recent records are returned; run links remain
available even if the engine no longer retains a result in its live view.

A crash during dispatch remains ambiguous. Such events are shown as requiring
review and are never automatically replayed. A delivery receipt does not imply
that execution or a notification succeeded. Credentials in displayed free text
are scrubbed, but events may contain personal mail metadata, so authentication
and the owner-only database permissions are required.

## Migration and rollback

`scripts/migrate_event_watch.py` provides an offline, backed-up migration for one
legacy alias; pass its explicit old database path and `--daemon-stopped`.

Stop the daemon before moving a legacy watch. Split its connection/scope into a
source and its condition/target into a rule, keeping the legacy alias as the rule
ID. Remove that legacy channel entry and its agent binding. Move the old
`events.db` into `<data_dir>/events/<rule>/` with SQLite backup while no listener
is running. Preserve the original config, database and binary for rollback.
Keeping the same effective policy and connection preserves the source revision
and mail baseline; changing them intentionally resets it. Do not delete a
baseline to test notification delivery.

To pause monitoring, disable the source or rule in Events. To roll back the
binary, restore the matching pre-migration configuration and event store while
the daemon is stopped. Runs already admitted remain governed by SOP lifecycle.

Verification includes the isolated daemon fixture:

```sh
cargo test -p zeroclaw-events --lib
cargo build --features channel-event-watch,channel-wechat
python3 scripts/ci/event_watch_smoke.py --native --binary target/debug/zeroclaw
```
