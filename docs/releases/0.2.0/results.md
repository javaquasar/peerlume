# PeerLume 0.2.0 implementation results

Date: 2026-11-05 (Europe/Berlin).

Base: `2c5cf1a5495606f162676052b069b96374ab7b7d`. These results describe the
implementation candidate for the [versioned plan](release-plan.md), not a
published release. Measurements were captured from the local working tree before
publication. Cargo package metadata still identifies version 0.1.0;
the 0.2.0 documentation identifies the planned application release.

Compiler used locally: Rust 1.93.0, commit
`254b59607d4417e9dffbc307138ae5c86280fe4c`, Windows x86_64 MSVC, LLVM 21.1.8.
The installed `stable` toolchain has this exact version; local commands selected
it explicitly without changing the repository toolchain pin.

## Implementation status

All seven workstreams have implementation and semantic verification. Comparative
measurements and their limitations are recorded below.

| Item | Implemented behavior | Evidence |
| --- | --- | --- |
| PERF 1 | Release-default Windows/Linux launchers; synthetic workload and process CPU/RSS sampler | Python and PowerShell argument tests; preserved baseline executables and source digests; Release build |
| PERF 2 | Engine hash-to-handle index replaces repeated full scans | Actual-engine recheck, removal, failed/stale registration and re-add tests |
| PERF 3 | Expiring file/peer detail leases, cheap exact aggregate progress, capped peer materialization | Hidden-detail and boundary-piece tests; fresh reliable file snapshot before selection ACK |
| PERF 4 | Virtual GUI torrent rows and shared retained view cache | 5,000-row viewport/last-ID selection test; sort/filter invalidation and existing interaction regressions |
| PERF 5 | Adaptive frontend refresh and bounded background metainfo import | GUI/TUI rendering tests; blocked import cancellation, stale result and ordered review tests |
| PERF 6 | Shared volume admission for checks and copy/cleanup, configurable check limit and bulk I/O rate | Actual-engine admission/shutdown test; cross-volume, cancellation, queue bounds, shared rate and checksum tests |
| PERF 7 | Dirty-only resume checkpoints and changed-child persistence | SQL no-write checks, rollback/retry, child-row triggers and credential-provider regressions |

## Important implementation details

### Launch and measurement

[Everyday launch and workload commands](performance-workload.md) explain the
Release default, explicit development mode, argument forwarding and required
PowerShell/Python versions. The existing per-profile owner lock remains the
authority; launchers do not rewrite profiles or start a duplicate engine.

The local optimized GUI, TUI, daemon and adapters were built with:

```text
cargo +stable build --locked --release --target x86_64-pc-windows-msvc --all-features --bins --example perf-workload
```

Executables are under `target/x86_64-pc-windows-msvc/release/`. This explicit target
reuses the existing local build cache. Set `CARGO_BUILD_TARGET` to that triple when
using the launcher with this cache. The ordinary portable launcher default also
supports Cargo's normal `target/release` location.

### Engine and detailed observations

Hash lookup follows current engine registration, including replacement handles
after recheck. Cumulative network totals are not reset when a live generation
changes. File/peer detail interest expires after three seconds and retains at
most one selected frontend interest. Aggregate counters and actual completion
continue without a Files or Peers pane. File counts in the model remain the last
genuine detail snapshot; summary progress does not invent their distribution.

The real engine maintains exact selected-file verified bytes, including partial
file-boundary pieces. File selection emits a reliable `FileSnapshot` before
`FilesApplied`, so an acknowledged selection cannot persist a total derived from
stale hidden detail. Public IPC still excludes paths, peers and credential text.

### Presentation and imports

GUI rows are generated only for the viewport. A 5,000-torrent synthetic rendering
test observes fewer than 60 torrent names and can select the last authoritative
ID after scrolling. Search keys and sorted indices are retained. A cheap O(N)
comparison of relevant inputs remains; this is not a claim of zero per-frame work.
Column measurements invalidate on relevant metrics, fonts and display density.

GUI refresh uses 50 ms for pending work, 250 ms for active transfers/checks and
2,000 ms for idle/minimized views. Native input can wake the GUI sooner. TUI still
polls input every 100 ms but does not redraw solely because a poll elapsed.

Metainfo import uses one worker, bounded request/result channels, one replaceable
pending request and at most 64 queued GUI review paths. It checks cancellation
between 32 KiB reads and after bounded parsing; the UI never joins a blocked read.
An operating-system open/read that is already blocked cannot be forcibly aborted;
the single worker can remain occupied until that call returns. Cancellation
discards its result, and loading never adds a torrent without normal review.

### Coordinated disk work

Checks, verified copy and explicit duplicate cleanup share admission by pinned
filesystem volume. Windows uses the handle's volume serial, Linux uses `st_dev`;
unknown identity conflicts with every volume. Different volume identities do not
prove different physical disks. Queued work does not create an OS worker thread.

`queue.max_active_checks` now reaches the actual engine initializer (1..32).
`storage.max_active_bulk_jobs` defaults to 1 (1..32), and
`storage.bulk_io_limit_kib` defaults to 32768 KiB/s; 0 disables bulk throttling.
There are bounded waiting queues (32 checks and 64 bulk jobs). The shared bulk
rate charges reads, writes and checksum read-back, so payload copy throughput can
be lower than the nominal I/O budget. It does not throttle ordinary peer download
writes or every operating-system cache operation. Cancellation remains cooperative.
Checksums, file synchronization, no-overwrite rules and move recovery are retained.

### Persistence

Repeated unchanged resume checkpoints return without row updates. Only successful
transactions advance the last-saved cache; failure retries remain dirty and removal
purges cache entries. Batch statements are prepared and SQLite `synchronous=FULL`
is unchanged. Scalar saves avoid rewriting unchanged files, trackers, piece maps
and audit records. They still read/compare child records; no elimination of all
database reads or large-tree acknowledgement cost is claimed.

Unchanged tracker URLs use in-memory SHA-256 fingerprints to retain their existing
protected references without reopening the OS keystore for every acknowledgement.
The cache retains no plaintext URLs. Normal credential restoration still fails
closed. Replacing a lost reference with a newly protected URL remains atomic.

## Local validation

The following completed successfully on Windows; counts include repeated cases
across feature configurations and must not be added as a unique-test count.

| Configuration | Passed | Ignored |
| --- | ---: | ---: |
| All features, all targets, application and tracker library | 637 | 10 |
| Core only, all targets | 503 | 7 |
| TUI only, all targets | 553 | 8 |

The all-feature library subset contains 579 passing application tests and 21
passing tracker-library tests. Integration suites cover core workflows, adapter
failures, protected credentials and copied installed CLI/MCP executables.

A subsequent GUI follow-up makes downloaded-data recycling remove the list entry
in the same confirmed operation. Its six new disposable-filesystem regressions
passed together with all existing GUI tests (67 passed, one installed-fixture
case ignored). Strict all-feature Clippy and the rebuilt Release GUI also passed.
The full-matrix counts above precede that follow-up. The workload does not execute
GUI recycling, and its five measured source inputs are unchanged by this fix.

Also passed: strict all-target Clippy in all three feature configurations,
formatting/diff checks, headless dependency-graph checks and documentation tests
(zero runnable documentation cases), 26 Python launcher/package/sampler tests,
PowerShell literal-argument checks, and actionlint. The final English-only scan
passed for 408 text files and 424 filenames (16 binary files skipped).

Ignored tests require Docker, explicitly installed/native-window fixtures, a
bounded full-disk runner, long workloads or an opt-in external tracker. They were
not silently enabled on a user's profile. The vendored engine is not a workspace
test package, so its direct `-p librqbit` unit-test invocation is unavailable;
the equivalent selected-boundary and replacement behavior passed through actual
engine tests in the application suite.

## Measurements

See [measurement methodology](performance-workload.md), the
[comparison and sampling evidence](comparison.md), and the complete raw reports
under [measurements/](measurements/). Baseline executables were preserved before
optimized linking. Their compiled input hashes match the base commit; optimized
development/Release input hashes match each other. Each run uses 200 view
iterations, 20 checkpoint iterations, 1/10/100 torrents with 100 files each, and
the same 100,000-file model workload.

Release operation measurements for 100 torrents (p50 / p95):

| Operation | Baseline | Optimized |
| --- | ---: | ---: |
| Name-sorted presentation | 16.7 / 16.8 microseconds | 2.0 / 2.1 microseconds |
| Unchanged resume checkpoint | 1.4428 / 1.5340 milliseconds | 0.0109 / 0.0158 milliseconds |
| Changed resume checkpoint | 2.8935 / 5.5651 milliseconds | 1.9979 / 4.1686 milliseconds |

The presentation comparison uses the former stateless frontend call versus the
new retained frontend cache for identical unchanged inputs. It measures the
actual caller improvement, not a guarantee that a fresh stateless API call is
allocation-free. These are short wall-clock samples, not swarm throughput.

An initial whole-process comparison showed slower optimized wall time. That
result is retained rather than discarded. Three subsequent back-to-back Release
pairs, including reversed execution order in pair 2, produced:

| Pair | Baseline wall / CPU (ms) | Optimized wall / CPU (ms) | Baseline / optimized sampled RSS peak (bytes) |
| --- | ---: | ---: | ---: |
| 1 | 640.842 / 343.750 | 556.101 / 203.125 | 19,714,048 / 20,398,080 |
| 2 | 626.275 / 281.250 | 566.544 / 203.125 | 19,939,328 / 20,262,912 |
| 3 | 635.816 / 343.750 | 552.506 / 218.750 | 19,705,856 / 19,841,024 |

The earlier wall-time regression did not persist in these paired runs. CPU and
the targeted operation costs improved in the measured workload; reliable RAM
savings are **not established**. Sampled RSS peaks were slightly higher in each
pair. No claim of universal startup, real-client idle CPU, disk throughput or
download-rate improvement follows from these small synthetic measurements.

## Remaining release gates and limits

This local verification does not run Linux binaries, native-window OCR workflows,
an installer upgrade, Docker fixtures or a new long soak. Linux implementations
and CI contracts are present, but their results require a subsequent real CI run.
No source push, tag or published release is implied by these local results.

The synthetic benchmark opens no transfer session, tracker, peer or user profile.
Its CPU time and sampled working set include its fixture/model work and resident
shared pages. Sampling misses shorter peaks; start/tail/max gaps are reported.
It is not a heap allocation profile, a client throughput test, proof of lower RAM
for every swarm, or closure of the independent continuous 24-hour release gate.
