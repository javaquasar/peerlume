# PeerLume 0.2.0: performance and responsiveness

Plan revision: 1.1

Created: 2026-11-05 (Europe/Berlin)

Status: implemented and locally validated on Windows. See [results](results.md)
and [measurements](comparison.md) for evidence and outstanding platform gates.

Revision history: revision 1.0 was saved before implementation. Revision 1.1
records completion of all seven workstreams, including shared check/bulk
admission, reliable file-selection snapshots and credential-safe persistence.

Baseline source: `2c5cf1a5495606f162676052b069b96374ab7b7d`.

Target: application release 0.2.0. This document does not publish a release,
create a tag, or imply that package metadata has already changed.

## Objective

Implement all seven agreed optimization areas while preserving torrent identity,
actual transfer accounting, file-selection semantics, verification, durable
acknowledgements and Windows/Linux frontend parity. Reduce avoidable polling,
allocation, rendering and disk work before considering protocol changes.

The current local interactive executable is a development-profile build. CI
packages already use the release profile. File rows are already virtualized;
file-view results, bounded live journals and coalesced telemetry already exist.
This release must improve the remaining paths rather than duplicate those features.

## Delivery principles

- Use synthetic disposable profiles and loopback fixtures for measurements and
  tests. Do not benchmark against production trackers or mutate user payloads.
- Keep one controller/engine owner per profile. Changes to lookup and interest
  tracking must survive pause, repair, recheck, removal and replacement handles.
- A queued request remains different from an engine acknowledgement. Persistence
  failures must remain visible; do not acknowledge durable success prematurely.
- Preserve control-event ordering, backpressure and bounded retained state.
- Keep checksum read-back, payload synchronization, path confinement, move recovery,
  transaction boundaries and SQLite durability settings intact.
- Do not restart an interactive client merely to collect a baseline. Everyday
  launch tools should default to release builds without altering profile contents.
- Report observed measurements and their limits. Do not invent throughput gains,
  RSS reductions, a completed 24-hour gate or proof that all leaks are absent.

## PERF 1: release launch and repeatable measurements

Deliver Windows and Linux launch commands that select release builds by default,
with an explicit development option and ordinary argument forwarding. Avoid a
second application instance and preserve normal profile-lock enforcement.

Add a deterministic, headless performance workload covering 1, 10 and 100 torrents,
large file models, repeated view operations and persistence checkpoints. Record
the source/build identity, workload sizes, repetitions and elapsed measurements
in a machine-readable report. Separate CPU/allocation hypotheses from resource
samples and wall-clock microbenchmarks. Compare development and release profiles
on the same machine when feasible; document unavailable measurements explicitly.

Acceptance:

- A documented command builds/runs release on both operating systems.
- A benchmark never starts a real download or opens a user's profile.
- Reports are reproducible inputs plus actual measurements, not hard-coded gains.
- Any new optimization flags are justified by comparison; changing flags alone
  does not constitute acceptance of the other six workstreams.

## PERF 2: indexed engine polling

Replace the repeated full-engine scan in the real transfer-metrics provider with
a hash-indexed handle lookup. Keep the index synchronized with actual engine
registrations and removals, including replacement handles during recheck/repair.
Do not cache a live generation in a way that resets cumulative network counters.

Acceptance:

- Periodic polling does not search the full engine collection once per torrent.
- Existing parallel-download, removal, repair, source-retry and accounting tests
  retain their behavior.
- A regression covers removal/replacement and prevents stale handle accounting.

## PERF 3: demand-driven detailed telemetry

Separate inexpensive transfer totals from per-peer and per-file detail. Detailed
interest belongs to frontend consumers; GUI/TUI request details for the selected
torrent and relevant pane, with expiry or explicit release to prevent abandoned
interest. Core correctness and aggregate selected progress must not depend on an
open Files or Peers pane.

Bound peer work before materializing an arbitrarily large frontend snapshot.
Avoid copying complete file-progress vectors for inactive torrents on every tick.
Keep coalescing and ordered reliable control events already implemented.

Acceptance:

- Hidden detail panes do not cause continuous complete peer/file snapshots.
- Opening a detail pane obtains a fresh snapshot in a bounded interval.
- Selected-file totals, completion, repaired files and persisted actual counters
  remain correct when no frontend requests detailed telemetry.
- Tests distinguish reduced producer work from merely bounded receiver backlog.

## PERF 4: virtualized and cached torrent presentation

Virtualize the main egui torrent list while keeping all columns and scroll extents
reachable. Cache normalized search/name keys and filtered/sorted presentation
indices by the data that affects their result. Reuse a view within a frame.
Cache column measurements with appropriate invalidation for data, font and width.

Acceptance:

- Render work scales with the visible viewport, not all matching torrent rows.
- Search, sort, live status/rate changes and empty filters invalidate correctly.
- Selection and context-menu commands remain tied to authoritative torrent IDs,
  including a reorder between pointer press and release.
- Existing narrow-window, divider, scrolling and full-name-cell tests pass.

## PERF 5: adaptive frontend work and background import

Replace unconditional frequent GUI/TUI redraws with event-driven or adaptive
refresh. Preserve prompt feedback for commands, transfers, checks, graphs and
pending operations; idle frontends may update less frequently. Reduce avoidable
work while minimized or on unchanged views where the framework permits it.

Read and parse torrent metainfo on a bounded background worker. Keep dialog review,
file-drop queue ordering and explicit add confirmation. Reject stale completions,
show loading/errors and support cancellation without retaining an unbounded task
or result queue. Do not hold the shared controller owner lock while waiting for
slow metainfo I/O.

Acceptance:

- Idle loops do not redraw the complete frontend at a fixed high frequency.
- Actual changes and input still produce timely visible feedback.
- A delayed or failed import does not stall the frontend or add a torrent itself.
- GUI/TUI command and installed-frontend regressions remain valid.

## PERF 6: coordinated disk work

Expose and apply the maximum concurrent integrity checks already represented by
queue settings. Introduce shared bounded admission for relocation/copy/cleanup
work instead of unrestricted active work per torrent. Coordinate by storage
volume where supported, with an honest conservative fallback when device identity
cannot be established. Avoid pretending that path strings identify physical disks.

Provide cancellation while waiting and a bounded copy I/O rate or configurable
budget so checks and copies can coexist with downloads. Preserve all existing
read-back, no-overwrite, confinement, recovery and explicit-cleanup guarantees.

Acceptance:

- Configured check concurrency reaches the actual engine initializer.
- Conflicting bulk disk jobs are queued; cancellation/shutdown cannot deadlock.
- Copy progress and failure semantics remain accurate while throttled/queued.
- Tests cover admission, cancellation and integrity without user directories.
- Document that a bulk-I/O budget does not automatically limit every socket or
  operating-system disk-cache write performed by the transfer engine.

## PERF 7: dirty-only SQLite persistence

Keep the five-second transaction checkpoint, but update only changed cumulative
resume counters. Track the last successfully persisted values; a failed commit
must not mark them clean. Remove cached entries when torrents are removed or
storage is replaced. Use prepared statements for batch writes.

Avoid full file/tracker rewrites for acknowledged changes that only update
settings or state when the child records are unchanged. Preserve credentials,
transactional rollback and the canonical persisted file tree.

Acceptance:

- Repeating an unchanged checkpoint performs no row updates.
- Changed rows commit atomically, survive reopen and retry after failure.
- No UI draft or unacknowledged state is persisted by the optimization.
- SQLite synchronization and payload synchronization remain unchanged.

## Implementation sequence and ownership

1. Save this plan and release-documentation index before changing source code.
2. Capture/reuse the baseline and implement independent workstreams in parallel:
   engine/telemetry/disk, frontend/view/import, release tooling/measurement, and
   persistence. Agree shared interfaces before modifying overlapping files.
3. Integrate contracts for detail interest, import state and disk settings.
4. Run focused semantic regressions, then the full local supported test/lint matrix.
5. Build the optimized release executable and capture the same workload report.
6. Record actual results, unfinished external-platform checks and operational
   limitations in this directory. Update each workstream's status honestly.

## Validation matrix

Required local checks where supported:

- `cargo fmt --all -- --check` and `git diff --check`.
- All-feature library/integration tests and strict all-target Clippy.
- Core-only and TUI feature compilation/tests to catch frontend coupling.
- Existing parallel swarm, continuous-engine repair, pause/error feedback,
  stale-removal, relocation, persistence and rendered UI regressions.
- Repository English-language checks for new code and documentation.
- New performance workload with exact inputs and recorded build profile.
- Release GUI build; script validation for launch argument handling.

Windows and Linux CI/package/native-window results are reported only after a run
actually exists and is inspected. No push, release publication, privileged runner
setup or unattended long experiment is implied by the existence of this plan.

## Evidence and release decision

Create `results.md` containing commit/base identity, validation outcomes, benchmark
commands and measured values, relevant resource limitations, and each PERF item's
completion status. Machine-readable reports may be stored as evidence artifacts;
do not commit user-specific profiles, absolute user payload paths or credentials.

The performance release is implementation-complete only when all seven items
have concrete behavior and appropriate verification. A final release/tag remains
a separate distribution action. Existing 24-hour and cross-platform reliability
gates retain their own evidence requirements.
