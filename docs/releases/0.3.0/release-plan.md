# PeerLume 0.3.0: efficient ownership and background operation

Plan revision: 1.0

Created: 2026-11-05 (Europe/Berlin)

Initial status: planned. This document was saved before source implementation
began. Implementation decisions, validation and remaining acceptance gates are
recorded in [results.md](results.md).

Baseline: `3d8242fd0e34347e287e693f224de9873ff97e4f`.

Target release: 0.3.0. A versioned implementation plan is not a release tag or a
claim that distribution metadata or release acceptance has already changed.

## Objective and existing foundation

Implement all seven agreed follow-up improvements: scalable metainfo validation,
indexed controller/event processing, event-driven idle ownership, narrow durable
updates, deferred cold file models with memory evidence, asynchronous bounded
journaling with rotation, and GUI/TUI attachment to a persistent daemon.

Version 0.2.0 already added engine hash indexing, expiring detailed telemetry,
virtual torrent rows, retained sorting, background import, coordinated disk work,
dirty resume checkpoints and Release launch/measurement tools. Those remain the
foundation. The previous synthetic runs did not establish RAM savings; this plan
requires direct evidence for its new memory behavior instead of assuming it.

## Invariants

- One authoritative controller, engine and database writer own a profile.
- Preserve torrent identity, real transfer totals, selection, priorities, mode
  acknowledgement, pause intent, repaired-piece accounting and completion events.
- A queued mutation is not an acknowledged or durably completed mutation.
- Preserve SQLite FULL synchronization, transaction rollback, payload checksums,
  fsync, path confinement, bounded queues and causal event ordering.
- Keep the existing restricted agent/CLI/MCP protocol separate from trusted local
  frontend access. Do not add names, paths, peers or secrets to public observations.
- Never silently clear or overwrite canonical file records from a deferred model.
- Run tests and measurements only with disposable profiles and synthetic/loopback
  data. Do not restart or migrate a running user's client during implementation.
- Retain unsuccessful measurements and report missing platform/resource evidence.
  No throughput, RAM or leak-absence claim follows merely from a successful build.

## OPT 1: scalable metainfo path validation

Remove the repeated all-previous-files prefix scan in relative-path validation,
using the maintained file/directory sets or an indexed equivalent. Prove that both
collision directions remain rejected regardless of input order. Avoid allocating
a cloned prefix for each candidate comparison.

Acceptance:

- Reject duplicate paths, file/directory prefix collisions, case aliases,
  Windows reserved/unsafe names, absolute paths and traversal exactly as required.
- Cover reordered parent/child paths, mixed separators, Unicode case conversion,
  limits and malicious input through deterministic and generated regressions.
- Measure validation/import at 1k, 10k and 100k paths. A bounded baseline timeout
  is recorded as a timeout rather than hidden or replaced with a smaller case.
- Keep parser/resource bounds and cooperative import cancellation intact.

## OPT 2: indexed controller and ordered event processing

Maintain canonical ID/alias lookup for controller event application. Update only
the observation associated with an event instead of scanning every torrent.
Reconcile identity membership only when structural inputs change; frontends must
not reorder authoritative IDs when sorting a presentation.

Replace the telemetry map's minimum-sequence scan on every pop with an ordered
index. Replacing a telemetry value must remove its old ordering entry; reliable
control events and verification/selection/shutdown barriers retain causal order.

Acceptance:

- Batched processing does not perform a full torrent search for every event.
- Same-length replacement, removal, re-add, aliases and stale events are covered.
- Telemetry coalescing, reliable backpressure and receiver-close shutdown remain
  bounded and do not leak obsolete ordering entries.
- Record event-drain/ACK measurements for 100, 1k and 5k synthetic torrents.

## OPT 3: event-driven owner work

Wake the owner when engine events, frontend mutations, access changes or IPC
requests arrive. Retain explicit timers for checkpoints, operation deadlines,
bounded maintenance and observed live clocks. Remove unconditional full owner
work merely because 20 ms elapsed.

Acceptance:

- An idle owner does not reconcile or publish a complete unchanged session at 50 Hz.
- Requests, state changes, token/access changes and shutdown wake promptly.
- Deadline expiration and periodic durable checkpoints still happen without a UI.
- A notification arriving near a wait boundary cannot be lost.
- Measure idle wake/work counts, process CPU and command latency honestly;
  benchmark counters are not substituted for a complete allocation profile.

## OPT 4: narrow acknowledged SQLite updates

Use explicit persistence intents for state/settings acknowledgements, selection,
priorities, storage and new registration. Pause/Resume and scalar mode/settings
updates must not load all file paths, verified pieces and audit rows merely to
preserve them. Existing full record insertion remains available for a new torrent.

Acceptance:

- Scalar acknowledgement persistence does not SELECT or rewrite the file tree.
- File selection/priority changes update only their authoritative fields and do
  not persist an incomplete frontend cache as a complete tree.
- Cold and warm credential behavior, rollback, durable success and move intent
  acknowledgement remain correct; unavailable storage remains visible.
- Reopen and SQL-trigger regressions preserve children, counters and credentials.
- Measure scalar ACK cost and database work with a 100k-file record.

## OPT 5: cold-model memory and deferred file detail

Measure retained model allocations/working set before changing representation.
Represent inactive torrent lists with bounded summaries and compact selection
metadata; load large file paths/details on demand and release cold copies when
they are no longer needed. Engine metadata retention is accounted separately.

Define explicit loaded/deferred states. Counts, selected sizes and progress must
remain correct without a full file vector. Operations that require paths or a
complete selection must request authoritative details, not infer an empty tree.
Use bounded/paged transport when detail exceeds one IPC frame.

Acceptance:

- A paused/inactive torrent can be listed without retaining its full frontend
  file-path tree, while selected totals and identity remain correct.
- Files views, search, priority edits, selection, export, resume and deletion load
  or fetch their required records safely and report pending/failure explicitly.
- Closing/switching a view can release cold detail; stale generation results do
  not overwrite a newer selection or a removed/re-added torrent.
- Restart and persistence cannot erase files because a model was deferred.
- Record comparable synthetic CPU/RSS and accounted component sizes; report
  allocator/sampling limits and any regression instead of claiming universal RAM savings.

## OPT 6: asynchronous bounded journal and rotation

Move serialization/file writing outside the shared live-view mutex onto a bounded
writer path. Establish precise accepted/queued/written/flushed states and shutdown
barriers; overflow is backpressure or an explicit error, never silent record loss.

Rotate disk segments by a documented size policy and maintain an ordered manifest
so export can stream the complete retained history across rotations. Retention
must be explicit; defaults must not delete accepted history invisibly. Disk-full,
partial writes and recovery gaps must be visible in status/export metadata.

Acceptance:

- Slow journal I/O does not hold the live-view mutex or block ordinary rendering.
- Reliable ordering, sequence uniqueness, live-tail bounds and complete export
  survive concurrent recording, segment rotation and clean shutdown.
- Tests cover a slow writer, bounded admission, failed writes, rotation/reopen,
  interrupted final records and missing segments without guessing success.
- Public redaction and credential rules remain unchanged.

## OPT 7: persistent daemon with trusted GUI/TUI attachment

Allow native frontends to attach to one existing daemon for a profile and to
start it in the background when appropriate. Closing/disconnecting a frontend
must not stop transfers. Explicitly stopping the background owner remains a
separate action with final counter persistence and profile-lock release.

Use a distinct local-only trusted frontend channel protected by current-user
endpoint permissions and bounded, authenticated framing. The existing external
agent protocol remains restricted and disabled by default as before. A second
frontend must not create a second engine or independently write SQLite.

Define versioned bounded snapshot/detail paging, mutation/ACK reconciliation,
disconnection, owner restart and stale revision semantics. Include names/paths
only in this trusted native boundary. Do not expose arbitrary shell execution,
unbounded file reads or a wider public deletion API.

Acceptance:

- GUI and TUI can observe/control the same disposable daemon-owned session.
- Frontend exit/restart preserves engine identity, live transfer continuity and
  cumulative actual counters; reconnect restores authoritative current state.
- The daemon owns grants, revisions, durable state and disk budgets. Frontends
  do not bypass ownership merely because they are local.
- A locked/incompatible legacy owner is reported clearly; never kill it or
  fall back to starting another engine in the same profile.
- Background launch is hidden on Windows, detaches terminal I/O appropriately,
  forwards profile identity literally and has bounded startup/error handling.
- Tests use copied executables and isolated profiles, including two frontends,
  disconnect/reconnect, explicit shutdown, authentication failure and oversized
  or stale requests. Preserve native actions and trusted confirmation behavior.

## Delivery order and shared contracts

1. Save this plan and add it to the release-documentation index before code edits.
2. Capture small/large baseline workloads with exact source/build identity.
3. Agree controller lookup, persistence intent, deferred-detail and trusted
   frontend transport contracts before parallel edits to shared ownership code.
4. Implement independent validators/journal, controller wake/ordering, storage/
   deferred models and daemon/frontend workstreams with scoped file ownership.
5. Integrate frontend and persistence boundaries before declaring any item complete.
6. Run focused regressions, the supported feature matrix and Release builds.
7. Record measurements, operational behavior, remaining external platform checks
   and all seven completion decisions in `results.md` in this directory.

## Validation and evidence

Required local checks where supported:

- Formatting, diff/English checks, actionlint and script contract tests.
- All-feature, core-only and TUI test/lint configurations; existing installed
  adapters, real parallel download, repair, removal and persistence regressions.
- New path-collision, ordered-event, idle-wake, scalar-update, deferred-state,
  writer/rotation and daemon-attachment integration tests.
- Release GUI/TUI/daemon/adapter builds with explicit source identity.
- Comparable synthetic workload reports, including errors/timeouts and sampling
  coverage. Keep earlier evidence; never overwrite captures to hide a regression.

All native/platform claims require actual execution evidence. Linux CI, native
window automation and a new continuous 24-hour soak remain distinct gates; local
Windows success does not silently close them. No push, tag, installer upgrade or
long unattended experiment is implied by creating this plan.

## Completion and limitations

All seven items require working behavior and verification, not placeholders or
settings that only serialize. Record compatibility/migration and operational
instructions, especially how to stop a background owner and how legacy owners
are detected. The running user's application is not migrated automatically.

A published release, package-version bump and release tag are separate actions.
Any incomplete acceptance condition is named explicitly rather than marked done
because the other optimizations passed.
