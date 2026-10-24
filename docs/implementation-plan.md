# PeerLume reliability implementation plan

This is the primary implementation plan from 2026-10-24. It takes precedence
over delivery ordering in the historical release plan. Keep the six identifiers
stable and attach implementation and GitHub verification evidence to each item.
Core library publication remains deferred.

## REL 1 Durable operation retention

Archive completed operations automatically instead of stopping normal automation
after 512 lifetime commands. Keep the active set bounded, query archives in pages,
and retain client-scoped idempotency keys and results across restart. Never evict
unresolved operations or turn a retry into a second engine command. Enforce an
explicit archive capacity and fail closed if persistence or that capacity fails.

Acceptance: more than 512 sequential operations succeed; active saturation still
returns Busy; archived retries, client isolation, rollback and restart are tested.

## REL 2 Long running resource tests

Provide a deterministic, isolated multi-torrent workload with slow, corrupt and
disconnecting local peers, observer reconnects, rechecks and interrupted resume.
Sample resident memory, threads, open handles, journal/database size and response
latency into JSONL. Check bounded growth after warmup and preserve failure evidence.
Run a short smoke test in ordinary CI and longer manual or scheduled runs.

Acceptance: CI executes the smoke workload and the manual workflow supports an
hours-long run without production trackers, user profiles or unbounded capture.
A 24-hour release gate requires a real completed 24-hour run.

## REL 3 Risk based regression coverage

Expand tests for torrent completion, progress and ETA, settings validation,
blocklist cache corruption and update failures, and frontend state transitions.
Use hostile input, boundaries and state invariants rather than test-count targets.
Measure Linux core/TUI coverage separately from Windows GUI behavior.

Acceptance: new branches have regression tests; coverage artifacts identify
remaining gaps without excluding difficult application code to improve a score.

## REL 4 Frontend and installed agent workflows

Exercise real engine confirmations and SQLite through egui, ratatui and an external
control process. Cover addition, pause/resume, restart, verification and damaged
piece repair. Render frontend state during the scenario and compare it with the
controller and persisted state. Distinguish deterministic in-process UI rendering
from native-window automation; do not claim one proves the other.

Acceptance: supported frontend configurations run the shared workflow in GitHub;
external adapter tests run copied executables with isolated profiles.

## REL 5 Filesystem and credential boundaries

Replace path-check-then-open at agent import/export boundaries with directory
capabilities and no-follow opens. Reject links, reparse points and overwrites;
test directory replacement against pinned handles. Preserve Windows DPAPI and add
a Linux Secret Service provider with injectable test stores. Never use plaintext
fallback or silently restore a redacted credential as a usable tracker URL.

Acceptance: secure-store round trips, unavailable/locked stores, legacy missing
secrets, redaction and path replacement have deterministic tests. Full protection
of backend payload I/O requires the backend to consume pinned directory handles;
record any unsupported boundary explicitly rather than claiming race immunity.

## REL 6 Continuous quality gates

Compare measured coverage with a checked-in baseline, run parser fuzz smoke tests,
retain regression corpora and failure artifacts, and schedule longer fuzz/resource
jobs. Pin tools and actions. Run tests on GitHub for every implementation push.

Acceptance: coverage loss and malformed baselines fail the gate; fuzz targets
compile and run in CI; scheduled/manual resource jobs publish machine-readable
results. A green smoke job is not evidence of a completed hours-long experiment.

## Delivery evidence

Implementation is in progress. Record actual run URLs and measured results after
GitHub jobs complete. Leave acceptance criteria open where evidence is incomplete.
