# 0.3.0 synthetic measurements

Measured on Windows x86_64 on 2026-11-05. Validation/import became substantially
faster for these fixtures. Journal live reads and producer admission improved,
while complete export became slower. Loaded-to-cold model transitions released
accounted buffers and reduced sampled RSS in this build. These are scoped
measurements, not a universal throughput or memory-leak conclusion.

## Source and build identity

The frozen baseline executable's four embedded input hashes match Git blobs at
`3d8242fd0e34347e287e693f224de9873ff97e4f`. Its SHA-256 and unsuccessful timeout
cases remain in [baseline-release.json](baseline-release.json).

The optimized candidate is a dirty worktree, not a nonexistent new commit.
All 285 source/build/assets/tool inputs were identical before Release compilation
and after every measurement, with aggregate SHA-256
`6d35f25ec85a63b5f207053ec9b0aab079898578b3a3c517c355f6bd27c3ec61`.
See [source-inputs.json](source-inputs.json),
[source-inputs-after.json](source-inputs-after.json) and
[release-build-identities.json](release-build-identities.json). The latter records
the exact build command, compiler, five application binaries, workload example
and ignored-test executable hashes. Every embedded optimized-example source hash
matches the full candidate manifest.

## Validation and import

The same path strings, counts and encoder are used. Operation times exclude
fixture generation; full process wall times remain in the raw JSON. These
fixtures parse metadata and perform no tracker traffic or downloads.

| Operation | Files | Baseline | Optimized |
| --- | ---: | ---: | ---: |
| Validate relative paths | 1,000 | 59.351 ms | 0.6339 ms |
| Validate relative paths | 10,000 | 5,604.9803 ms | 5.7972 ms |
| Validate relative paths | 100,000 | Child timed out at 15 s | 59.0643 ms |
| Parse/import metainfo | 1,000 | 58.5303 ms | 1.5028 ms |
| Parse/import metainfo | 10,000 | 5,542.2721 ms | 13.6127 ms |
| Parse/import metainfo | 100,000 | Child timed out at 15 s | 141.0887 ms |

Both baseline 100k children retain `completed=false` and `timed_out=true`.
Their full process walls were 15.0128014 s and 15.0143922 s. Exact operation
durations and exact 100k speedup ratios are unknown. Completed import cases have
matching encoded lengths: 49,090 and 490,090 bytes. Optimized 100k metainfo is
4,900,090 bytes. See [optimized-release.json](optimized-release.json).

## Journal responsiveness and export regression

Each run records 128 observations containing 512 peers while another thread makes
1,000 live-page reads. Every export was parsed completely and verified to contain
exactly 128 contiguous sequences. Producer admission does not establish disk
durability; completed export/barrier cost is measured separately.

| Initial comparable run | Baseline | Optimized |
| --- | ---: | ---: |
| Live-page p50 | 0.6 us | 0.6 us |
| Live-page p95 | 59.7 us | 0.8 us |
| Produce 128 records | 8.9942 ms | 1.1618 ms |
| Complete export | 7.7646 ms | 33.9364 ms |
| Producer plus export | 16.7588 ms | 35.0982 ms |

The initial export regression is retained as the primary observation. Three
additional alternating paired controls confirmed the tradeoff. Pair 2 reversed
execution order; all results are kept, without selecting a best run.

| Pair | Run order | Baseline export | Optimized export | Baseline produce+export | Optimized produce+export |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | Baseline, optimized | 8.2256 ms | 27.3349 ms | 17.5022 ms | 28.6333 ms |
| 2 | Optimized, baseline | 7.9074 ms | 27.1855 ms | 17.2197 ms | 28.3432 ms |
| 3 | Baseline, optimized | 8.1987 ms | 27.4941 ms | 17.6168 ms | 28.6604 ms |

Paired producer times were 9.2766-9.4181 ms baseline versus 1.1577-1.2984 ms
optimized; page p95 was 61.1-61.3 us versus 0.8-1.1 us. Complete export and the
combined produce/export path became slower. This measures responsiveness gains
alongside the cost of the new complete-history/barrier path; it does not claim
higher journal throughput. Scheduling affects read/write overlap, and these
short synthetic samples are not production tracker latency statistics.

Raw paired records:
[1 baseline](journal-pair-1-baseline.json), [1 optimized](journal-pair-1-optimized.json),
[2 baseline](journal-pair-2-baseline.json), [2 optimized](journal-pair-2-optimized.json),
[3 baseline](journal-pair-3-baseline.json), [3 optimized](journal-pair-3-optimized.json).

## Cold model buffers and sampled process resources

Each child holds three phases of the same model: loaded paths/tree/view, compact
cold owner, then summary-only mirror. Counts and selected sizes remain identical.
This compares representations inside the integrated build; it is not a
reconstructed pre-commit process-RAM baseline.

| Files | Loaded accounted buffers | Cold-owner accounted buffers | Summary per-file buffers | Loaded sampled peak RSS | Cold sampled peak RSS | Summary sampled peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 | 113,412 B | 16,250 B | 0 B | 4,325,376 B | 4,259,840 B | 4,243,456 B |
| 10,000 | 1,122,537 B | 162,500 B | 0 B | 5,783,552 B | 5,533,696 B | 4,579,328 B |
| 100,000 | 11,533,787 B | 1,625,000 B | 0 B | 18,239,488 B | 6,520,832 B | 4,890,624 B |

Each model is still a 208-byte inline object. Buffer accounting excludes fixed
cache allocations, allocator bookkeeping, engine metadata and SQLite. Zero
summary per-file buffers does not mean a zero-byte object or process. RSS is
sampled working set, not a heap allocation profile; allocator/OS retention can
affect how released buffers appear in RSS.

Each of nine phases has 20 successful samples at requested 10 ms intervals.
Observed maximum gaps were 11.022-11.111 ms, initial unsampled gaps 10.6-34.6 us,
and trailing gaps 10.069-11.015 ms. Shorter spikes can be missed. Process-lifetime
high-water values also include earlier phases and are separate from phase RSS
peaks. Whole-child CPU was 31.25 ms in each case, with walls 674.7942, 673.6164
and 702.3142 ms including held sampling periods. CPU counters have finite
granularity; these totals do not isolate the CPU cost of a single transition.
See [cold-model-resources.json](cold-model-resources.json).

## Empty owner observation

A copied Release daemon ran one empty disposable profile, with discovery,
extra trackers, UPnP, uTP and blocklist updates disabled. After one second of
warmup, the 2.0010848 s idle window reported 0 additional user/kernel CPU ns in
GetProcessTimes. This is a finite, granular counter observation, not a claim of
literally zero CPU use. Sampled peak RSS was 12,455,936 bytes.

There were 20 successful samples at requested 100 ms intervals, maximum gap
101.056 ms and unsampled tail 91.213 ms. Native attach took 181.3979 ms including
frontend process startup plus handshake/snapshot. Explicit Stop took
1,150.209 ms including CLI startup, shutdown and lock wait. Neither value isolates
IPC latency or measures scaled Pause/Resume acknowledgement latency. Owner
identity remained unchanged; actual owner/Stop exit codes were zero and its
journal manifest was clean. Listener binding remained the engine default,
not OS network isolation. See [idle-owner-resources.json](idle-owner-resources.json).

## Controller and scalar persistence

The optimized-only controller microbenchmark directly applies 20,000 synthetic
events to an offline controller, then calls the unchanged-publication fast path
20,000 times. This is not a complete transport drain or real-engine ACK latency
measurement.

| Torrents | Direct apply of 20k events | 20k unchanged publishes |
| --- | ---: | ---: |
| 100 | 9.202 ms | 0.052 ms |
| 1,000 | 10.359 ms | 0.052 ms |
| 5,000 | 10.619 ms | 0.052 ms |
| 10,000 | 11.892 ms | 0.052 ms |
| 100,000 | 16.698 ms | 0.052 ms |

The file-backed 100k-file scalar persistence fixture completed 20 FULL-durable
acknowledgements in 28.1176 ms, median 1.3766 ms and range 1.2215-1.6513 ms.
SQLite reported 100 profiled statements, 1,860 VM steps, zero fullscan steps,
zero canonical-child read/write preparation authorizations and 40 scalar-row
changes. All 15 aborting child-write guards remained intact. SQL authorization
counts are preparation access requests, not rows or physical disk reads.
Instrumentation overhead is included; setup and post-verification are excluded.
There is no matching historical ACK baseline or speedup claim. See
[controller-events.json](controller-events.json) and [scalar-ack.json](scalar-ack.json).

## Evidence gaps

- Historical RAM and idle-CPU baselines were not captured for these new cases.
- Controller and SQLite measurements are optimized-only. Full end-to-end scaled
  transport/engine ACK latency is not established by these microbenchmarks.
- Linux resource/lifecycle reports and interactive GUI/TUI window operation need
  external platform verification; native copied-executable probes are not manual
  visual testing.
- These short synthetic runs do not close a 24-hour gate, prove production/WAN
  throughput or establish absence of every memory leak.

See [README.md](README.md) for reproduction commands and
[validation-windows.json](validation-windows.json) for the separate semantic
test/lint evidence. Raw unsuccessful baseline/timeouts and the initial journal
regression remain preserved.
