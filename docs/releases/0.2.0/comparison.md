# Performance comparison: 0.2.0 implementation

Measured on 2026-11-05, Windows x86-64, Rust 1.93.0. Baseline source:
`2c5cf1a5495606f162676052b069b96374ab7b7d`. Optimized source is the integrated
0.2.0 working tree, not a published release. Raw evidence is in [measurements](measurements/).

## Inputs and identity

Every run uses 1, 10 and 100 synthetic paused torrents, 100 files per torrent,
200 repeated view operations, 20 unchanged and 20 changed checkpoints per case,
and construction/filtering of a 100,000-file model. No transfer engine, production
tracker or user profile is opened. The benchmark never launches, stops or attaches
to an interactive client. Ambient desktop activity was not controlled.

The development and Release baseline reports have identical compiled-input
digests, verified against the baseline Git blobs. Optimized digests match the
current source files and match across both profiles and all optimized runs.
Resource reports include executable SHA-256 values. The profile's debug-assertion
bit, exact workload inputs, child exit/completion and executable hashes were checked.

The baseline repeatedly invokes the original stateless view API; optimized
frontends and workload retain `ViewCache`. Sorted authoritative IDs, result
allocation reuse and one rebuild for unchanged inputs are verified. This measures
the intended caller change, not a claim about allocation-free stateless calls.

## Original operation samples

These are the original unsampled before/after reports for the 100-torrent case.
Values are **microseconds per operation**, shown as p50 / p95. Each row represents
one workload run; the percentiles describe its internal repetitions.

| Profile | Operation | Baseline p50 / p95 | Optimized p50 / p95 |
| --- | --- | ---: | ---: |
| Development | Name-sort view | 50.6 / 53.5 | 16.3 / 16.4 |
| Development | Unchanged checkpoint | 3636.9 / 4176.5 | 66.7 / 71.2 |
| Development | Changed checkpoint | 5230.2 / 5856.6 | 2474.1 / 4809.1 |
| Release | Name-sort view | 16.7 / 16.8 | 2.0 / 2.1 |
| Release | Unchanged checkpoint | 1442.8 / 1534.0 | 10.9 / 15.8 |
| Release | Changed checkpoint | 2893.5 / 5565.1 | 1997.9 / 4168.6 |

An unchanged checkpoint now avoids SQLite writes/transactions. These specific
operation timings do not imply the same speedup for total application runtime.
Raw [development](measurements/optimized-development.json) and
[Release](measurements/optimized-release.json) reports retain all three case sizes.

## First process-resource captures

CPU is complete child user-plus-kernel time. RSS is the largest successful
working-set sample, not heap usage. Baseline has one resource run per profile;
the optimized implementation initially had three per profile.

| Profile/run | CPU ms | Process wall ms | Sampled RSS peak bytes |
| --- | ---: | ---: | ---: |
| Development baseline | 812.500 | 1326.266 | 21626880 |
| Development optimized 1 | 656.250 | 1225.453 | 21966848 |
| Development optimized 2 | 671.875 | 1277.975 | 21336064 |
| Development optimized 3 | 656.250 | 1277.386 | 21331968 |
| Release baseline | 296.875 | 729.524 | 20045824 |
| Release optimized 1 | 203.125 | 772.464 | 19746816 |
| Release optimized 2 | 281.250 | 813.581 | 20033536 |
| Release optimized 3 | 234.375 | 858.024 | 21037056 |

The first Release captures have **higher wall time despite lower CPU time**.
These observations are retained. They motivated the additional paired experiment
below rather than a claim that total wall time had improved.

The original captures are [development baseline](measurements/baseline-development-resources.json),
[Release baseline](measurements/baseline-release-resources.json), and optimized
[development 1](measurements/optimized-development-resources.json),
[development 2](measurements/optimized-development-repeat-2-resources.json),
[development 3](measurements/optimized-development-repeat-3-resources.json),
[Release 1](measurements/optimized-release-resources.json),
[Release 2](measurements/optimized-release-repeat-2-resources.json),
[Release 3](measurements/optimized-release-repeat-3-resources.json).

## Alternating Release pairs

The preserved baseline executable and final optimized Release executable were run
back to back, with no build between runs. Pair 2 reversed their order. Every child
used the same 200 repetitions. Both sides' raw reports and all earlier reports
remain available; no result was discarded.

| Pair and actual order | Variant | CPU ms | Wall ms | Sampled RSS peak bytes |
| --- | --- | ---: | ---: | ---: |
| 1: baseline, optimized | [Baseline](measurements/paired-1-baseline-release-resources.json) | 343.750 | 640.842 | 19714048 |
| 1 | [Optimized](measurements/paired-1-optimized-release-resources.json) | 203.125 | 556.101 | 20398080 |
| 2: optimized, baseline | [Baseline](measurements/paired-2-baseline-release-resources.json) | 281.250 | 626.275 | 19939328 |
| 2 | [Optimized](measurements/paired-2-optimized-release-resources.json) | 203.125 | 566.544 | 20262912 |
| 3: baseline, optimized | [Baseline](measurements/paired-3-baseline-release-resources.json) | 343.750 | 635.816 | 19705856 |
| 3 | [Optimized](measurements/paired-3-optimized-release-resources.json) | 218.750 | 552.506 | 19841024 |

In these three pairs, optimized CPU and wall time were lower. The earlier wall
regression did not persist in the paired captures. Ambient activity and filesystem
cache/durability costs were not fully controlled, so this small experiment does
not establish statistical confidence or attribute all variation to one cause.

**No stable RAM reduction was demonstrated.** Sampled RSS was slightly higher in
each optimized paired run. Working-set sampling and allocator/page residency
variation do not establish a heap regression or the absence of leaks.

## Sampling coverage and limits

Across the nine optimized resource runs (three development, six Release), the
requested interval was 10 ms, with 49 to 116 successful samples per child.
First-sample gaps were 21.2225 to 27.2303 ms; maximum successful-sample intervals
were 11.1029 to 11.2496 ms; unsampled tails were 0.0441 to 0.1049 ms.
All resource queries succeeded, children exited with zero, and workload identity
and input checks passed. Each resource JSON retains its individual counts/gaps
and observed OS high-water mark. The latter is separate from the sampled peak.

The original baseline first-sample gaps were 38.0380 ms development and 26.1933 ms
Release. Final CPU counters include CPU before those first samples. Shorter RSS
spikes may be missed, and Windows working set includes shared and private pages.
No allocation profile was collected. Linux sampler support is implemented but
these measurements were executed only on Windows.

This synthetic workload does not measure GUI frame latency, real swarm throughput,
verification/copy bandwidth, cold startup or a continuous 24-hour process.
Those behaviors retain their separate test and profiling requirements.

The 32 raw JSON reports contain no private absolute paths; executable/report names
are basenames and compiled source names are relative. Reports were copied without
changing measured values. See [workload and sampler methods](performance-workload.md)
for commands, API definitions and remaining measurement limits.
