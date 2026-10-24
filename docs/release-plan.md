# PeerLume release plan

The [reliability implementation plan](implementation-plan.md) is the primary
implementation order from 2026-10-24. This document retains the broader roadmap
and historical delivery evidence; it does not supersede the six REL priorities.

This plan takes PeerLume from the current desktop prototype to a production-oriented
BitTorrent client. Each milestone keeps formatting, strict Clippy, unit, property,
integration, and container tests green. Network features remain disabled in the UI
until deterministic local tests pass.

Tracker passkeys are never stored as plaintext. On Windows, the full Tracker URL is
protected with current-user DPAPI and restored for the matching user after restart.
The ordinary SQLite URL column, logs, errors, and status messages contain only a
structurally redacted URL.

This is a living plan covering the client milestones, the
[installed application agent control track](#installed-application-agent-control-track),
and the deferred [library publication track](#library-publication-track).
A working application or green application CI does not by itself establish library
release readiness. Local agent control does not depend on publishing a library.
Add new requirements as they emerge, and mark work complete only with linked
implementation and verification evidence.

## Milestone 0.2: hostile-input boundary

### 1. Resource and path limits

Use one `ResourceLimits` policy for metainfo, tracker responses, magnet metadata,
blocklists, resume state, file trees, peer messages, and UI imports. Enforce byte,
depth, node, file, tracker, path, content-length, decompression, and rule limits.
Reject Windows aliases, reserved names, alternate streams, duplicate paths,
symlink/junction escapes, and file/directory collisions. Revalidate final paths
immediately before creation.

Done when hostile input fails before unbounded allocation or filesystem mutation.

### 2. Property and fuzz testing

Property-test bencode, tracker responses, cache decoding, path normalization, piece
mapping, peer messages, magnet metadata, and resume migrations. Maintain fuzz targets
and retain discovered failures as a regression corpus.

Done when arbitrary bytes cannot panic and every parser has bounded allocation.

## Milestone 0.3: complete transfer engine

### 3. Multi-peer, multi-piece session

Implement one cancellable task per torrent and bounded tasks per peer. Support
bitfield, have, choke, unchoke, request, piece, cancel, keepalive, timeouts,
reconnect backoff, request pipelining, and endgame duplicates. Publish real progress,
rates, peers, errors, and ETA through bounded event channels.

Done when several deterministic peers jointly serve a complete multi-piece torrent.

### 4. Piece scheduling and selective download

Support rarest-first and sequential strategies, Skip/Low/Normal/High priorities,
first/last-piece priority, availability counts, in-flight ownership, retries, and
endgame replication. Map selected file ranges to pieces and write only selected
segments after the complete piece passes its hash check. Preserve v2 padding.

Done when boundary pieces are verified without writing skipped file ranges.

### 5. Storage, resume, and disk pipeline

Use positioned I/O, bounded disk jobs, free-space checks, optional preallocation,
sparse files, safe temporary names, atomic completion moves, an open-file limit, and
a bounded upload read cache. Checkpoint verified pieces, counters, priorities, and
uncertain pieces in one SQLite transaction. Recheck only uncertain data after restart.

Done when interrupted transfers resume without redownloading trusted pieces.

### 6. Memory and throughput controls

Use a reusable block-buffer pool, bounded request counts and channels, immediate
buffer release after verify/write, batched UI events, and batched SQLite writes.
Keep positioned I/O as the default; add memory mapping only after measurements.

Done when large swarms and 100,000-file torrents stay within configured memory.

## Milestone 0.4: trackers and discovery

### 7. Tracker lifecycle

Implement HTTP/HTTPS and UDP announce, started/completed/stopped, tracker tiers,
failover, interval/min-interval, warnings, failure reasons, scrape, IPv4/IPv6 peers,
transaction validation, and exponential retry. Counters reflect verified payload;
disabling reporting disables tracker traffic.

Done when HTTP and UDP container trackers pass lifecycle and failure tests.

### 8. Magnet metadata and extension protocol

Implement extension handshakes and per-peer IDs, `ut_metadata` request/data/reject,
bounded assembly, duplicate handling, and info-hash verification. Open the normal
file-selection preview after verified metadata arrives.

Done when a magnet-only session retrieves verified metadata from local peers.

### 9. DHT, PEX, and LSD

Implement IPv4/IPv6 DHT and KRPC, PEX, and local discovery. Apply blocklist,
connection limits, and source attribution to every peer. For `private=1`, never
start these providers and evict non-tracker peers if metadata reveals privacy later.

Done when policy tests prove zero discovery traffic for private torrents.

### 10. BitTorrent v2 and hybrid torrents

Support SHA-256 Merkle verification, piece layers, file roots, padding files, v2
identifiers, and hybrid swarm selection. Reject inconsistent hybrid metadata.

Done when v1, v2, and hybrid fixtures produce identical selected output.

### 11. Network policy and reachability

Add SOCKS5 and HTTP tracker proxies, optional peer proxying, interface binding,
IPv6, listening ports, UPnP/NAT-PMP/PCP, encryption policy, connection and rate
limits, and deterministic token buckets. Check blocklists before connecting and
evict newly blocked peers after updates.

Done when bind, leak, rate, port mapping, and live blocklist tests pass.

## Milestone 0.5: persistence, security, and desktop workflow

### 12. SQLite state model

Use WAL, foreign keys, transactional migrations and checkpoints. Persist identity,
source metadata, redacted trackers, file selection and priority, overrides, queue,
counters, verified pieces, blocklist metadata, labels, schedules, and UI state.
Never persist tracker passkeys in plaintext; protect the exact URL with an
OS-user-bound secret mechanism and keep the ordinary tracker column redacted.

Done when migration, rollback, cascade, concurrent read, and crash recovery pass.

### 13. Safe filesystem behavior

Check free space before start and file extension. Prevent symlink and junction
traversal, ensure canonical parents stay under the destination, limit open handles,
distinguish list removal from data deletion, and move completed data atomically.

Done when adversarial paths and interrupted moves cannot escape the destination.

### 14. Complete add and file-selection UI

Support picker, drag-and-drop queue, folder import, clipboard magnets, watch folders,
Windows file/protocol association, and sequential previews. Provide a hierarchical
tree, tri-state folders, search, sorting, priorities, and shared-piece details.

Done when 100,000-file imports and previews remain responsive.

### 15. Torrent management UI

Add categories, labels, destinations, drag queue ordering, status filters, context
menus, per-torrent limits, schedules, notifications, completed-file moves, and
sequential mode. Peers, Trackers, Pieces, Files, and Log show real engine data.

Done when the full workflow operates without blocking the UI thread.

### 16. Diagnostics and privacy

Use structured redacted logs with bounded rotation, event history, database health,
tracker timing, blocklist freshness, peer source, request latency, disk queue depth,
and buffer usage. Diagnostics exports exclude paths and credentials by default.

Done when automated secret scans find no passkey in persistent output.

## Milestone 0.6: release verification

### 17. Integration, interoperability, and soak testing

Expand testcontainers fixtures with HTTP/UDP trackers and slow, choking, corrupt,
disconnecting, and wrong-info-hash peers. Cover multi-piece, multi-file, private,
resume, v1/v2/hybrid, padding, proxy, blocklist, and low-disk cases. Test against
Transmission and a libtorrent client. Run Windows/Linux CI and a 24-hour soak.

Done when one documented command runs the deterministic release suite.

The tracker accounting threat model, anomaly matrix, false-positive controls, and
acceptance criteria are specified in `tracker-audit-scenarios.md`.

## Installed application agent control track

Status: in progress; all six AGENT tracks now have implementation slices,
including installed local CLI/MCP access. See the 2026-10-24 delivery evidence and
remaining gates below, and [the installed control guide](agent-control.md).
Decision recorded on 2026-10-23: implement agent
control as an isolated internal module in this project. Do not publish the core as
a library, create a standalone SDK, or require a crates.io release for this work.
The library track remains a future roadmap, not a prerequisite or current delivery
commitment. All six capabilities below remain in scope; read-only access is the
first delivery slice, not the final feature set.

The installed application must expose its existing session to authorized agents.
Agents must not create a second transfer engine, independently edit SQLite, or
compete with the UI for the session's single event receiver. GUI, TUI, CLI and MCP
must use one controller and observe the same confirmed state.

### Internal module boundary

Use a provisional `src/control/` module, private to the application crate where
possible. Keep controller/state ownership, wire contracts, operations, permissions,
IPC and agent adapters behind a small internal facade. Split responsibilities into
submodules as needed; this is not a commitment to the final filenames or a public
Rust API. Existing transfer and persistence components remain behind the controller.

GUI and TUI call that facade. CLI and MCP adapters translate external requests into
the same commands; they do not contain separate download or persistence logic.
Packaging an installed CLI or MCP bridge binary is allowed within this repository
without extracting or publishing a library crate. Code extraction can be reconsidered
later without making it part of the agent-control acceptance gates.

### AGENT 1 Establish one authoritative controller

- [x] Move authoritative torrent models and session-event reduction out of UI-owned
  state into one controller around the existing session and persistence services.
- [x] Give the controller one transfer session and one session-event consumer.
- [ ] Route GUI, TUI and external commands through the same validation, application,
  confirmation and persistence path.
- [ ] Expose snapshots and bounded per-client subscriptions instead of allowing
  multiple clients to consume the original session receiver.
- [ ] Define revisions and ordering so simultaneous user and agent changes have
  explicit conflict behavior rather than silently overwriting each other.
- [ ] Coalesce telemetry for slow subscribers, expose event gaps, and permit
  snapshot recovery. A stalled agent must not block the UI or grow memory without
  bound; terminal operation results must remain queryable separately.

Done when GUI and an attached control client see the same confirmed torrent state,
only one engine owns each download, and disconnecting one observer does not steal
events from or stall another observer.

Implementation slice, 2026-10-23: the [controller](../src/control/mod.rs) owns the
backend and torrent models for egui and ratatui. Each running frontend owns one
controller; cross-process GUI/TUI attachment still requires the future IPC layer.
Both frontends poll notices from their controller rather
than consuming raw session events or applying the reducer themselves. The existing
canonical reducer remains shared; observations project its results rather than
implementing separate completion or file-selection accounting rules.

The internal `ReadHandle` retains one latest compact observation, with independent
reader cursors, monotonic revisions, explicit resynchronization, revision-pinned
pagination (at most 128 torrents per page), and controller-unavailable reporting
after shutdown. It has no event/history queue per observer and publishes nothing
for unchanged observations. Confirmed engine observations exclude local mode drafts,
restored-but-unconfirmed counters, tracker URLs, paths, names, peer addresses and
free-form errors. This minimal projection is not yet the full AGENT 3 contract.
Payload progress uses the canonical model's current local file selection; it is
not an acknowledgement of a selection-change command. Network totals are separate.

Verification: [controller tests](../src/control/tests.rs) cover concurrent/slow/dropped observers,
pagination bounds and conflicts, JSON safety, integrity-check states, selected-file
accounting, mode acknowledgements and a real isolated session continuing after one
reader disconnects. The fixture uses local files with trackers and discovery off.

Command gateway slice, 2026-10-23: [existing-torrent commands](../src/control/commands.rs)
now route GUI and TUI pause/resume, recheck, reporting-mode, repair-policy,
post-download verification and file-selection requests through shared validation
and typed transport errors. A successful submission returns `AcceptedCommand`, not
an applied setting, a completed operation or a verified download. Both UIs leave
confirmed state unchanged while a request is queued. A mode draft may display
Applying or a submission failure, but does not replace the saved active setting.

Optional per-torrent `command_revision` preconditions reject stale submissions
before enqueueing. Tokens advance on registration and accepted commands, not
telemetry or commands for another torrent. Reconciled removal/re-registration gets
a new token. These in-process tokens are distinct from observation revisions and
do not provide restart-safe operation IDs, idempotency or result correlation.
Callers must not reuse them across controller lifetimes. Legacy import/removal
paths are not yet covered by this precondition contract.

The engine now explicitly acknowledges successful file-selection and
post-download-verification changes. The canonical reducer updates the model only
after those acknowledgements. GUI and TUI share controller-side persistence for
confirmed settings. Persistence requires a confirmed-settings marker and a real
state store; errors retain the marker for retry rather than claiming success.
Repair-enabled downloads retain required verification even if disabling it was
requested. Changing selected files invalidates a previous Done confirmation until
fresh engine progress arrives.

Batch pause/resume uses the same validated path, reports partial acceptance and
publishes one compact snapshot per batch. GUI file selection builds a request from
viewport edits without mutating or cloning the full file model on each frame.
Owner-side `poll_next` and `publish` also support in-project headless drivers;
read observers still cannot consume the session stream.

Verification: [command tests](../src/control/commands/tests.rs), frontend tests,
file-selection tests and checked-transport tests cover all seven command kinds,
alias normalization, conflicts, invalid metadata/indices, queue errors, bounded
batch publication, confirmation-gated SQLite persistence and persistence failure.
Property tests exercise arbitrary file selections. Local engine fixtures cover
pause/resume, mode/repair/verification/file acknowledgements and rechecking corrupt
data without inventing new network downloads. Frontend integration tests exercise
both successful and rejected dispatch rather than only mock engine events.

Historical remaining work at the end of the 2026-10-23 slice (superseded in part
by the 2026-10-24 delivery below): imports, removal, lifecycle ownership and other local
frontend preferences/audit persistence still use transitional crate-local access.
Authorization, durable command/result correlation, idempotency and coordinated
restart handling must be implemented before external mutations are enabled.
Frontend polling still drives reduction;
an independently driven controller lifecycle is not implemented. The read handles
are in-process polling observers, not IPC subscriptions or a durable operation
history. There is no Named Pipe listener, external CLI/MCP access, authorization
service or library publication in these slices. These unchecked gates remain open.

Lifecycle checkpoint slice, 2026-10-23: engine-confirmed Pause/Resume state changes
now emit a distinct `StateApplied` notice. GUI and TUI route these notices through
the same owner-side `persist_confirmed_update` gate as confirmed settings. Queue
acceptance and engine failures do not overwrite the last confirmed database state.
Failed checkpoints retain an in-process retry marker; unknown torrent state events
cannot create a checkpoint marker. Live telemetry still does not cause a database
write on every tick.

Resume checkpoints store active-transfer intent (`downloading`); restoration maps
it to WaitingForEngine and lets the new engine verify existing data before Done.
Pause checkpoints restore Paused rather than automatically resuming. Tests close
the old runtime and SQLite connection, reopen the database and start a new isolated
runtime, verifying both intents and preservation of the selected reporting mode.
Commands accepted without an engine acknowledgement are still not durable
operations, and interruption/result reconciliation remains a future gate.

CI evidence for the foundation and command-gateway slices: all five jobs passed
for commit `47d934d` in [GitHub Actions run 40016765319](https://github.com/javaquasar/peerlume/actions/runs/40016765319),
including Windows GUI/TUI, Linux core/TUI, Docker integration tests and strict
quality checks. Subsequent changes require their own verification evidence.

### AGENT 2 Add local IPC and installed client adapters

- [x] Start with a Windows Named Pipe endpoint restricted to the intended OS user.
  Define equivalent local transports for additional supported platforms separately.
- [x] Add instance ownership, endpoint discovery and a versioned handshake. Detect
  stale discovery records and refuse to attach to an incompatible instance.
- [x] Make external control explicitly enabled by the user; do not expose an
  unauthenticated network listener by default.
- [x] Add a scriptable CLI with structured JSON output and documented exit codes.
- [x] Add a thin MCP adapter with discoverable tool schemas. A stdio bridge may
  attach to IPC but must not create another SessionRuntime or own the download
  lifecycle. Keep protocol output on stdout and diagnostic output on stderr.
- [x] Return an explicit not-running result when no instance exists. Starting the
  application must be a separate, authorized action rather than a side effect of
  a read-only query.
- [x] Keep background/headless operation optional: attaching to an already running
  GUI is sufficient for the first slice; a system service is not required.

Done when installed CLI and MCP adapters attach to the same running application,
without repository source paths, a second engine, or direct database mutation.

### AGENT 3 Define a machine-readable control contract

- [ ] Version the protocol and publish capability discovery, request/response
  schemas, typed errors, stable torrent IDs, units and timestamp conventions.
- [ ] Support listing torrents and reading status, file selection, mode application,
  verification, recovery and operation results.
- [x] Distinguish tracker-returned peers from currently connected peers, actual
  counters from sent counters, and downloaded payload from verified completion.
- [ ] Support add/preview, pause, resume, recheck, mode changes, repair settings and
  journal export through the same controller used by the UI.
- [x] Keep data deletion separate from removing a torrent from the application.
  Do not expose arbitrary shell commands as an automation shortcut.
- [x] Return acceptance separately from application: enqueueing a command must not
  be represented as successful completion or an active reporting policy.
- [ ] Bound request sizes, result sizes, subscriptions and pagination. Let clients
  filter journals and peer lists without receiving an entire multi-hour history.
- [x] Expose only supported capabilities; do not advertise unimplemented settings
  or invent successful values for unavailable data.

Done when a client can complete the documented workflow using structured values,
without parsing UI labels, scraping screenshots, or inferring success from a timeout.

### AGENT 4 Make long-running operations and retries reliable

- [ ] Return an operation ID for rechecks, recovery and other asynchronous work,
  with explicit queued, running, succeeded, failed and cancelled outcomes.
- [x] Provide progress, result lookup, deadlines and cancellation semantics. State
  whether cancelling an observation also cancels the underlying operation.
- [x] Add idempotency keys bound to caller identity, command and normalized
  arguments. Reject reuse with different arguments rather than repeating writes.
- [x] Use expected revisions or equivalent conflict checks for competing mutations.
- [ ] Persist the operation records needed for reconnect/restart behavior, with
  bounded retention and explicit semantics for expired keys and interrupted work.
- [x] Do not automatically replay destructive or externally visible commands when
  their previous result is unknown. Return a reconciliation-required outcome.
- [x] Keep the application's transfer lifecycle independent of a bridge process:
  agent exit, pipe loss or MCP disconnect must not stop ongoing downloads.

Done when connection loss and retries cannot silently duplicate mutations, and a
client can distinguish completion, failure, interruption and an unknown result.

### AGENT 5 Enforce permissions and human approvals

- [ ] Default attached agents to read-only access and require explicit grants for
  managing downloads, changing test/reporting policies, exporting data or deleting
  payload files. Treat experimental audit traffic as a separately granted capability.
- [x] Authenticate local clients and enforce authorization in the controller, not
  solely in adapter descriptions or MCP tool annotations.
- [x] Require a real user approval for destructive actions through a trusted UI or
  equivalent human-controlled channel. An agent-supplied `confirmed=true` is not
  consent; bind any approval to the exact operation and reject altered requests.
- [ ] Restrict import, download and export paths, revalidate filesystem boundaries,
  and apply input/resource limits to every external entry point.
- [ ] Redact passkeys, tokens and secret Tracker URL fields from agent responses,
  operation errors and logs. Use secret references rather than returning credentials.
- [x] Treat torrent names, filenames and tracker messages as untrusted data, not
  instructions or authorization to invoke another tool.
- [ ] Record agent mutations with authenticated client identity, operation/request
  IDs, authorization decision and confirmed outcome, without persisting secrets.
- [x] Make granted access visible and revocable in the application. Do not install
  agent-host configuration or enable remote access without user approval.

Done when denied actions have no side effects, destructive actions cannot bypass
human approval, and privacy/resource limits hold for hostile local requests and
untrusted tracker metadata as well as ordinary interactive use.

### AGENT 6 Package and verify the installed workflow

- [x] Distribute adapters with the application and document executable discovery,
  connection setup, protocol compatibility and access revocation.
- [ ] Test an installed or packaged build in an isolated user profile, not only a
  development checkout with hardcoded repository paths.
- [ ] Test simultaneous GUI and agent control, conflicting requests, retries,
  slow/disconnected clients, application restart and stale instance metadata.
- [ ] Test IPC authorization, denied mutations, approval binding, secret redaction,
  malformed messages, oversized responses and path-boundary attacks.
- [ ] Test bounded subscriptions and paged long-running diagnostics without memory
  growth caused by an agent that stops reading.
- [x] Verify CLI JSON/exit-code contracts and MCP discovery/schema behavior using
  local deterministic fixtures, without contacting production trackers.
- [ ] Add an end-to-end acceptance scenario: connect an agent to a running app,
  read state, perform an authorized operation, observe its confirmed result,
  disconnect the agent, and verify that downloads continue and the UI agrees.

Done when the installed workflow passes without a second engine, direct SQLite
editing, GUI click automation, unbounded output, or library publication.

### Agent control delivery order

1. AGENT 1: establish controller ownership, shared state and bounded observation.
2. AGENT 2 and AGENT 3: add local IPC, structured read-only CLI and MCP access.
3. AGENT 4 and AGENT 5: establish retry/operation semantics and authorization before
   enabling mutations; start with explicitly granted pause/resume operations.
4. Extend the same contracts to adding torrents, verification, recovery, reporting
   policies and export; keep destructive actions behind bound human approvals.
5. AGENT 6: verify installation, concurrent use, reconnection and the full workflow.

Keep AGENT identifiers stable and link completed checkboxes to implementation and
test evidence. Add further requirements without replacing this internal-module
decision with an implied library-publication requirement.

### Installed control delivery, 2026-10-24

The six AGENT sections above remain release acceptance gates, not a claim that
every checkbox is closed by one delivery. Current implementation and limitations
are documented in [Installed agent control](agent-control.md).

- AGENT 1: independent owner pump, shared GUI/TUI/controller synchronization,
  common ordinary add/remove and existing-torrent commands, bounded observer and
  frontend notices, confirmation-gated persistence. Legacy preference/audit access
  and full lifecycle extraction remain open.
- AGENT 2: explicit session-only enablement, owner-profile lock before engine
  creation, current-user Windows Named Pipe DACL / mode-0600 Unix socket,
  `peerlume-control` and `peerlume-mcp`, instance/version discovery, no automatic
  launch or second engine. Both adapters are tested from copied executables.
- AGENT 3: typed version-1 request/response contract, discoverable input schema,
  paged reads for torrents/files/journal/tracker peers and operation results,
  confirmed vs actual/sent counters, authorized add/preview/manage/export. Unknown
  capabilities and connected peer addresses are not fabricated. Archived paging
  and a final complete response-schema publication remain open.
- AGENT 4: durable bounded operation records, distinct queued/running/terminal
  outcomes, per-client argument-bound idempotency, revision conflicts, deadlines,
  safe cancellation before dispatch and reconciliation after restart/ambiguity.
  Running-work cancellation remains open. The REL 1 implementation now archives
  terminal results automatically while preserving keys; active/archive capacity
  exhaustion rejects new work rather than silently forgetting idempotency keys.
- AGENT 5: hashed tokens, independent grants, controller-side authorization,
  trusted GUI/TUI enable/grant/revoke/approval controls, bound remove/export
  approvals, path checks, structurally redacted responses/exports and bounded
  authenticated action/state audit. No agent-grant or approval bypass method,
  remote listener, arbitrary shell or payload-deletion endpoint. Atomic
  handle-relative path hardening and payload-deletion approval design stay open.
- AGENT 6: copied CLI/MCP/profile integration tests, local real tracker/peer
  download after IPC disconnect without UI polling, authorization/retry/restart/
  export/property tests, and Windows development ZIP packaging in existing CI.
  Full graphical frontend + agent E2E, signed installer integration and broader
  hostile concurrent-filesystem testing remain open.

Implementation: [controller](../src/control/mod.rs),
[agent coordinator](../src/control/agent.rs), [local host](../src/control/host.rs),
[adapters](../src/control/adapters.rs), [operation store](../src/control/operations.rs).
Verification: [controller regressions](../src/control/agent_tests.rs),
[real transfer after disconnect](../src/control/transfer_test.rs),
[installed adapters](../tests/installed_adapters.rs). CI runs these through the
existing all-target Windows/Linux matrix; passing CI does not close the explicit
remaining release gates above. No library publication is part of this delivery.

## Library publication track

Status: deferred. Current decision, 2026-10-23: retain the core inside this project
and implement agent control as an isolated internal module first. No core library
publication is currently planned for execution. The milestones below remain a
future roadmap and do not gate installed-application agent control.

Initial readiness review: 2026-10-23, application commit
`63556d9`. If publication is resumed, the first target would be an experimental
library alpha, not a stable SDK.
No release date or final crate layout has been committed.

Publish PeerLume as an application-oriented core built on librqbit: torrent
management, reporting policy, verification and recovery, accounting observations,
and integration contracts. Do not describe it as an independent BitTorrent engine
or require a network-engine rewrite as part of library extraction.

### Current baseline

- [x] GUI and TUI rendering are separated from shared client and session logic.
- [x] Headless core builds are supported inside the repository.
- [x] Commands, events, persistence, verification, recovery and journals have tests.
- [x] Windows and Linux application CI and local Docker fixtures are available.
- [ ] A plain external consumer can build the core without repository overrides.
- [ ] A deliberately scoped public API and compatibility policy are established.
- [ ] Runtime, paths, persistence and secrets can be supplied by the embedding host.
- [ ] Package metadata, examples and artifact verification are release-ready.

These checked items describe existing infrastructure, not completion of the
library milestones. The initial external-consumer probe failed: a normal path
dependency with `default-features = false` selected the upstream
`librqbit-tracker-comms`, which lacks PeerLume's reporting and observation APIs.

### LIB 1 Resolve the dependency publication blocker

Priority: release blocker.

- [ ] Decide whether to upstream the required tracker hooks or maintain a
  consistently published fork dependency chain.
- [ ] Ensure PeerLume and librqbit use the same intended tracker implementation;
  registrations must affect the workers that perform real announces.
- [ ] Remove the requirement for consumers to copy PeerLume's root Cargo patch.
- [ ] Add a minimal external-consumer fixture and run it in CI outside the
  application workspace, with no inherited repository patch or Cargo config.
- [ ] Verify reporting policy and real announce observations through that consumer,
  not merely that its imports compile.

Cargo only applies patches from the consuming workspace root; patches declared
by dependencies are ignored. A direct path dependency or a renamed fork alone
must not be assumed to replace librqbit's transitive tracker dependency.
See the [Cargo dependency override rules](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section).

Done when a clean consumer builds and exercises the intended transport using the
distributable dependency graph, without requiring a consumer-side patch.

### LIB 2 Define crate boundaries and a small public API

Proposed structure, subject to a recorded design decision:

- `peerlume-core`: headless domain and session facade over the transfer backend.
- Optional SQLite persistence and platform secret-store adapters, behind features
  or in separate crates.
- An application package containing egui and ratatui frontends and desktop defaults.

- [ ] Decide the workspace and feature layout; keep UI dependencies out of the
  library's default dependency graph.
- [ ] Define a supported `Client` or `Session` facade with builder-based options.
- [ ] Introduce typed torrent identifiers, lifecycle states and library errors;
  avoid string-formatted IDs and string-only error matching as the public contract.
- [ ] Keep internal implementation modules private unless intentionally supported.
- [ ] Use extensible options and non-exhaustive types where appropriate, so adding
  a setting does not force consumers to rebuild large struct or enum literals.
- [ ] Avoid exposing backend-specific types where they would unnecessarily bind
  consumers to the current fork or backend version.
- [ ] Separate supported settings from modeled or planned settings. Return an
  explicit unsupported-capability result instead of implying an option was applied.
- [ ] Keep accurate reporting as the default and clearly distinguish experimental
  audit/reporting policies from ordinary transfer accounting.

Done when one documented public facade supports the common workflow without UI
types, internal module imports, or unsupported configuration assumptions.

### LIB 3 Make embedding and lifecycle behavior explicit

- [ ] Accept paths, journal configuration, persistence and resource limits from
  the embedding host; remove mandatory PeerLume data-directory conventions.
- [ ] Make file creation and persistence opt-in or explicit in builder options.
- [ ] Choose and document runtime ownership: an owned runtime, an existing Tokio
  runtime, or supported implementations of both.
- [ ] Report startup failures through a typed result or readiness handshake.
  Do not silently turn initialization failures into missing optional services.
- [ ] Provide cancellation and explicit shutdown suitable for async callers.
- [ ] Define Drop behavior without requiring a GUI event loop or unsafe assumptions
  about blocking inside a host runtime.
- [ ] Document event ordering, telemetry coalescing, control backpressure, receiver
  closure and the host's responsibility to drain events.
- [ ] Test stalled consumers, startup failure, cancellation, repeated session
  creation and shutdown from supported sync and async contexts.

Done when a standalone CLI or service can own the library's lifecycle and storage
without modifying process-global application settings or relying on UI polling.

### LIB 4 Define portability and secret-storage guarantees

- [ ] Introduce an injectable secret-store contract or equivalent explicit policy.
- [ ] Retain Windows DPAPI as an adapter rather than a universal library assumption.
- [ ] Define supported behavior on Linux and other target platforms before claiming
  that private Tracker URLs survive restart there.
- [ ] Return explicit errors or unsupported-capability results when credentials
  cannot be securely persisted; do not silently restore an unusable redacted URL.
- [ ] Test credential round trips, unavailable stores, user-binding failures,
  redacted exports and absence of secrets in errors and diagnostics.
- [ ] Publish a platform support matrix; do not infer macOS or other platform
  support from successful Windows and Linux application builds.

The original non-Windows assessment did not preserve full secret URLs. REL 5 now
adds an injectable credential provider, current-user DPAPI on Windows and Linux
Secret Service for file-backed profiles. Missing/locked providers fail explicitly;
legacy Linux records without their secrets require re-entering the original URL.
Other platforms and an embedding-host support contract remain library gates.

Done when supported platforms have tested, documented restart behavior and no
plaintext-secret fallback is introduced implicitly.

### LIB 5 Establish packaging and compatibility policy

- [ ] Choose an owner-approved project license and review obligations for vendored
  dependencies before distributing a standalone library package.
- [ ] Complete package metadata: description, repository, documentation, license,
  intended package contents and minimum supported Rust version.
- [ ] Set and test the MSRV rather than inferring it from the edition or one local
  build; exercise dependency resolution without relying solely on the app lockfile.
- [ ] Define the alpha stability policy, supported features, breaking-change rules,
  persistence/schema compatibility and event-format compatibility.
- [ ] Review the packaged file list for local downloads, credentials, generated
  output and unnecessary application-only resources.
- [ ] Add rustdoc and compiled examples for creation, adding a torrent, observing
  events, changing policy, handling errors and shutting down.
- [ ] Document backend limitations, experimental policies and dependency provenance.

Done when a new user can understand the support contract and build a documented
headless example from the package rather than from the development checkout.

### LIB 6 Verify and release the distributable artifact

- [ ] Run the external-consumer test against the packaged artifact and resolved
  release dependency graph, not just against workspace source paths.
- [ ] Verify minimal core, optional persistence, supported frontend combinations,
  MSRV and current stable Rust on the supported platform matrix.
- [ ] Run formatting, strict Clippy, unit, property, integration, container and
  documentation tests; retain the existing transfer and recovery regressions.
- [ ] Add library lifecycle and API-contract regressions, resource-bound checks,
  multi-session isolation and portability coverage.
- [ ] Inspect `cargo package --list`, perform package verification and a publish
  dry run in a clean environment. Resolve package failures rather than bypassing
  verification with `--no-verify`.
- [ ] Review release notes, supported capabilities, unresolved alpha limitations,
  security/privacy guarantees and license metadata.
- [ ] Obtain explicit approval for the package name, version, registry and actual
  publication. A documentation update or dry run is not publication authorization.

Done when the verified alpha artifact is reproducible, its clean-consumer checks
pass, and the approved publication can proceed without local overrides.

### Library delivery order

1. LIB 1: fix the dependency graph and add a failing-then-passing consumer test.
2. LIB 2: choose package boundaries and the intentionally supported public facade.
3. LIB 3 and LIB 4: define host integration, lifecycle and secret-store behavior.
4. LIB 5: finalize documentation, licensing, MSRV and compatibility expectations.
5. LIB 6: verify the artifact and prepare an explicitly approved alpha release.

### Open library decisions

- Dependency strategy: upstream hooks or a maintained published fork chain.
- Package layout: workspace crates versus optional adapter features.
- Runtime ownership and the sync/async embedding contract.
- Secret-store providers and the initial supported platform matrix.
- License, MSRV, package names and alpha version/stability policy.
- Whether experimental tracker-audit policies require a separate opt-in feature.
- Which real backend capabilities are guaranteed by the first public release.

### Maintaining the library plan

Keep LIB identifiers stable. Add new milestones with new identifiers rather than
renumbering existing ones. Add a date and decision rationale when resolving an
open question. Link completed items to commits, tests or verified artifacts, and
retain unresolved limitations in the release notes. Application CI success alone
does not close an external-consumer or package-publication gate.

The client delivery order and gates below remain a separate application track.

## Delivery order

1. Scheduler, selected-range storage map, checkpoints, and rate limiter.
2. Multi-peer v1 transfer using HTTP trackers.
3. UDP trackers, tiers, resume recovery, and real UI metrics.
4. Extension protocol and magnet metadata.
5. DHT/PEX/LSD with private-torrent enforcement.
6. v2/hybrid and network reachability.
7. Desktop completion, diagnostics, interoperability, and soak gates.

## Release gates

1. No parser panic or allocation outside configured limits.
2. No plaintext tracker passkey in persistent state, logs, errors, or diagnostics.
3. Private torrents emit no DHT, PEX, or LSD traffic.
4. Every completed piece is verified before trusted resume state.
5. SQLite migrations and checkpoints are transactional.
6. Blocked peers are rejected before handshake and evicted after updates.
7. UI commands are cancellable and perform no blocking network or disk work.
8. Memory, connections, requests, buffers, and queues have explicit limits.
9. Strict Clippy, formatting, unit, property, integration, and container tests pass.
10. Windows/Linux interoperability and the 24-hour soak pass before release.
