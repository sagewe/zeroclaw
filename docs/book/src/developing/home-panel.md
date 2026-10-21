# Private household panel

The `/home` dashboard is a local integration surface for the independent Rust
MiJia service. Its room grouping, multi-lens camera cards and combined activity
feed are informed by Miloco's household UI; the implementation uses ZeroClaw's
components, translations, authentication and existing integration configuration.

## State and permission ownership

- The MiJia service's live policy owns the selected home and explicit power-switch controls.
  Its SQLite database owns observed devices, properties and the recent event log.
- ZeroClaw's enabled, digest-pinned `mijia` binding owns the CLI executable and
  service root. The agent risk profile owns `mijia_cli` access. No credentials,
  device identifiers or permission lists are embedded in the frontend.
- The gateway selects the first enabled agent, in sorted alias order, with the
  explicit CLI grant. `deny_all_tools`, exclusions and `always_ask` deny this
  surface; it does not implement interactive approvals. Read-only autonomy blocks
  power changes. Legacy MCP bindings retain their per-tool approval checks.
- The MiJia policy's optional camera section owns the capture allowlist and
  worker/library paths. Absence disables capture. OAuth uses the service's
  existing encrypted account and refresh owner. Captures accept no arbitrary
  camera identifiers, worker paths, audio or PTZ settings.
- Native event rules, SOPs and execution history stay in their existing owners.
  The panel links to those views; it does not infer causality from nearby timestamps.

## Gateway contract

`GET /api/home` runs the CLI's `dashboard` command to read: service health, cached device
and power-switch observations, bounded property observations and the latest activity.
It does not reconcile cloud state or capture video. The frontend reads this
projection every ten seconds while visible and labels stale/disconnected data.

`POST /api/home/action` admits only four typed actions:

- `refresh`: explicitly synchronizes the directory and power-switch observations.
- `set_power`: forwards a named power switch and exactly `on` or `off`. The Rust service
  enforces its live allowlist, serializes writes and requires accepted receipt
  plus readback. The UI disables stale/unknown/unavailable controls.
- `device_spec`: queries the selected in-scope device's read-only MIoT definitions.
- `snapshot`: obtains two JPEGs on explicit demand. No capture runs on page load.

Both endpoints require existing gateway authentication. Before dispatch, the
backend resolves live grants, agent membership, executable and sandbox policy
at process dispatch. The legacy MCP path rechecks after its handshake. Responses use `Cache-Control: no-store`. Calls and
response sizes are bounded; error bodies do not expose upstream credentials.
Ambiguous control results are not automatically retried by the UI.

Grant `mijia_cli` only to the intended operator agent; the service continues to
own the explicit device-property allowlist and the camera scope. Camera access
uses this same operator grant; the standalone C500 MCP connection is removed.
Installations
without an enabled binding and the corresponding grant remain closed.

## User behavior and limits

Device rows group by room identity, not room display name. A details drawer
shows observed values with source/timestamps and official capability definitions.
An absent observation is not interpreted as off. Devices without control grants
remain read only. Camera images remain in page memory and retain their capture
time; previous images are not labelled fresh after an unsuccessful capture.

The activity feed shows matching rows within the service's most recent 50 log
entries. It combines property changes, online changes and completed control
results, including no-op confirmations. It is not a complete long-term archive.
The cloud device directory is reconciled explicitly; this release does not add
local-hub control, continuous camera perception, audio or new appliance grants.

## Validation and rollback

Focused tests cover action allowlisting, live permission/bundle revocation,
room identity and stale control guards. Service tests cover home-scoped projections.
Live acceptance additionally checks authenticated/unauthenticated gateway access,
actual cached observations, scoped no-op controls, the camera response, and the
rendered page on the installed deployment. No deliberate appliance toggle is
needed for acceptance.

Rollback restores the prior signed ZeroClaw binary and web distribution, together
with the prior MiJia binary if necessary. Remove only the newly added dashboard
read grant. Keep current device state, policy and encrypted credentials; avoid
restoring an entire old configuration. This work is private and is not published.

## Expanded household switch scope

The operator can approve a larger explicit device-property allowlist in the Rust
service's canonical policy. The four-control limit has been removed; the policy
admits 1 to 512 unique property targets, including multi-channel switches and
readable/writable boolean MIoT `on` properties. Existing aliases remain stable.
The dashboard displays the live count and generic power controls. This does not
grant arbitrary property writes, lock actions, PTZ or appliance program actions.
Offline or unknown states remain visible but cannot be toggled until confirmed.
New devices are discovered read-only until their property targets are explicitly
added to the service policy. Event subscriptions remain independently scoped.

## CLI transport

A pinned package can declare `[cli] protocol = "mijia_cli_v1"` instead of `[mcp]`.
The instance then binds absolute `cli_command` and `cli_root` paths and leaves
`mcp_server` empty. These operator-owned config paths are the only executable and
service-root authority; packages never supply executable commands. Existing MCP
packages keep their previous wire representation and connection requirements.

The `mijia_cli` native tool and household HTTP handlers share one typed argv
executor. Each dispatch resolves the enabled binding and live owning-agent grant,
checks the native runtime and existing sandbox policy, adds read-only access to
the configured executable and the service root's `policy.json` and `access.token`
for this invocation only, and spawns the configured CLI without a shell, bounds stdout/stderr, kills timed-out/cancelled children and does
not retry. These invocation-scoped file grants do not change generic shell or
file-tool permissions. The sandbox owner stays alive until the child exits, and
stdio/environment restrictions are applied after any replacing wrapper.
The tool requires explicit `auto_approve` and admission in any nonempty
`allowed_tools` list. Exclusion, `always_ask` and deny-all win. Read-only autonomy
also blocks power writes. Missing live config fails closed for executable tools;
registry-only discovery can still expose their descriptions.

Agent requests select finite commands (`devices`, `switches`, `show`, `spec`,
`read`, `power`, `events`, `status`, `refresh`, `dashboard`, `cameras`, `snapshot`). Arbitrary argv, roots,
program paths, shell fragments and background watches are not admitted. Targets
are single literal argv values after `--`; filter values use explicit option
assignments. The CLI accesses the Rust service's authenticated `/api/command`
JSON interface, not its legacy `/api/mcp` compatibility endpoint.

Push observation remains an independent WebSocket subscription. The existing
adapter receives an ephemeral connection projection built from the CLI service
root's private policy/credential files, re-resolved during authorization checks.
That projection is not registered as an MCP connection and creates no duplicate
credential config. The household source retains its explicitly selected entities.
Other integrations retain their own transports.

CLI approval belongs to the agent's risk profile. Agents sharing that profile
share its native-tool grants, even when their MCP bundles differ. During migration,
assign the household operator a dedicated policy before granting `mijia_cli` if
mail or other agents must retain narrower access. That policy becomes the operator's
canonical permission set, not a runtime mirror of another profile. Preserve the
existing sandbox, filesystem, command and delegation constraints when separating
these policies, and verify the other agents' effective tool registries.

## Camera service and CLI

`mijia cameras` returns the authorized camera metadata without capture.
`mijia snapshot --output NEW_DIRECTORY` saves two private JPEGs; `mijia --json
snapshot` returns a bounded JSON response with two inline images. The operator
must choose an output mode; the CLI never dumps base64 into a human terminal by
default. Existing output directories are rejected to prevent overwriting files.

The service executes the Rust camera worker with credentials on stdin, suppresses
SDK output, and decodes both lenses with its configured FFmpeg. The native Xiaomi
SDK remains inside the child process. A shared account lock serializes captures
and credential refresh. The service rechecks the live camera scope before capture
and delivery. Child processes have deadlines and are killed on cancellation;
private temporary video is removed on completion or failure. Abandoned capture
directories are recovered after the worker watchdog deadline.

The native tool converts only an explicit `snapshot` response into two declared
image attachments in the owning agent workspace, using the existing media
materializer. Image bytes are omitted from text/history payloads; saved attachments
follow existing workspace retention. The browser receives the same JSON via the
shared CLI executor and holds images in page memory. Camera capture remains
explicit and on demand; event monitoring never captures automatically.

Rollback can restore the private service source/binaries and prior signed runtime,
then remove the optional camera section and restore only the former C500 connection
and grants from the migration backup. Keep the current encrypted OAuth record.

### Native camera resolution

`mijia cameras` reports the service-owned capture profiles and default resolution.
`mijia snapshot --resolution 2560x1440 --output NEW_DIRECTORY` selects the high
native stream; `--resolution high` is an alias. `848x480` and `low` select the low
stream. Omission preserves the low default, including the household page.
The native agent tool accepts the same optional `resolution` field, for example
`{"command":"snapshot","resolution":"2560x1440"}`. Other commands reject that field.

The private service's shared camera profile module is the canonical mapping of
supported sizes to SDK quality values, reused by its CLI and isolated worker.
ZeroClaw forwards a bounded literal resolution option and discovers supported
profiles through `cameras`; it does not keep a second resolution table. SDK LOW
and HIGH were measured on both C500 lenses as 848×480 and 2560×1440 respectively.
The service reads actual JPEG SOF dimensions and rejects an unexpected fallback
size. It performs no spatial resizing. Snapshot responses include actual width
and height per image; the native tool preserves those values as frame metadata
alongside its two declared image attachments. Unsupported sizes fail before
capture without automatic fallback or retry.
