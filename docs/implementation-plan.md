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

The initial implementation is delivered in `4e8ccd7` and `271068c`.
The [application test matrix](https://github.com/javaquasar/peerlume/actions/runs/40077949609)
and [Reliability jobs](https://github.com/javaquasar/peerlume/actions/runs/40077949726)
pass for `271068c`. The coverage baseline is the measured `f0bc306` result;
the [Coverage workflow](https://github.com/javaquasar/peerlume/actions/workflows/coverage.yml)
publishes current JSON, LCOV and HTML reports and rejects a lower result.

The pinned backend storage, immutable agent input, native X11 scenario and
continuous-workload workflows were introduced in `284c15e`, with CI corrections
in `7a88af6` and `f104d04`. The
[Windows/Linux test matrix](https://github.com/javaquasar/peerlume/actions/runs/40081776040)
and [coverage gate](https://github.com/javaquasar/peerlume/actions/runs/40081776056)
pass for `f104d04`: Linux core/TUI coverage is 88.438% of lines and 80% of
functions. Native-window success additionally requires both GUI processes to
exit cleanly; destroying the X11 surface is not a successful shutdown.
A [four-hour hosted workload](https://github.com/javaquasar/peerlume/actions/runs/40082042583)
was dispatched for the same source revision. Its final JSONL success record,
not dispatch or a green smoke run, establishes completion of that experiment.

- REL 1: implemented hot/archive retention with a 512-record hot set and a
  100,000-record archive. Tests run 1,536 sequential operations and check archived
  lookup, client isolation, restart, rollback, saturation and preservation of keys.
  Archive quota exhaustion remains an explicit Busy result, not key eviction.
- REL 2: implemented the measured Linux resource workload and push/manual/weekly
  jobs. It holds one owner alive, reconnects IPC clients and repeatedly runs local
  parallel-download and corrupt-piece-repair fixtures in separate sessions.
  Sampling now runs once per second during activity as well as at idle cycle
  boundaries, with post-warmup RSS/thread/handle budgets for both idle values and
  sampled peaks. The separate Long soak workflow supports four hosted hours or
  one continuous 24-hour process on an existing trusted Linux self-hosted runner.
  Reports require a final success record. Allocation profiling, sustained churn
  within one swarm and a completed 24-hour run remain release gates.
- REL 3: added completion/ETA/ratio, blocklist corruption and legacy-header,
  settings, operation quota, missing-credential and startup-recovery regressions.
  Fixed legacy cache allocation before validation and rejected non-finite enabled
  ratio limits. Coverage gates retain the previous measured baseline.
- REL 4: implemented rendered egui and ratatui workflows against a copied external
  CLI, real engine acknowledgements and SQLite. They exercise frontend pause/resume,
  external policy/recheck operations, confirmed status rendering and restart.
  Added a Linux X11 native-window scenario using copied ordinary GUI/CLI binaries,
  mouse/OCR input, fresh local-peer download, GUI pause/resume, external recheck,
  corrupt-piece refetch and restart. Screenshots and a final result are retained
  in GitHub artifacts. Windows native input and broader visual parity remain open.
- REL 5: implemented pinned no-follow preview reads/export creation, bounded
  metainfo reads, injectable credential stores, Windows DPAPI and Linux Secret
  Service with a disposable real-service CI test. Missing secrets fail explicitly
  and both frontends display restore errors without clearing saved records.
  The real transfer backend now consumes a per-torrent pinned directory storage
  factory, uses owned file handles for positioned I/O and retains the capability
  across rechecks. Agent adds pass the validated metainfo snapshot directly to
  the engine without reopening the import pathname. Tests cover missing source,
  Unix path replacement through a real backend/recheck and Windows junctions.
  Human-controlled root resolution, restart, hard links and arbitrary same-user
  file modification remain outside a claim of complete filesystem race immunity.
- REL 6: implemented a measured coverage baseline with negative gate self-tests,
  five bounded fuzz targets, seed/failure corpus artifacts and weekly/manual
  fuzz/resource jobs. All five fuzz targets run successfully in Reliability CI.
  A smoke fuzz run is not exhaustive parser verification.

Continue with the open gates above. Do not mark the full release acceptance
criteria complete on the strength of smoke or rendered-only tests.
