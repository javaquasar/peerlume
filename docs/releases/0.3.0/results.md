# PeerLume 0.3.0 implementation results

Date: 2026-11-05 (Europe/Berlin).

Base: `3d8242fd0e34347e287e693f224de9873ff97e4f`. The detailed English
[release plan](release-plan.md) was saved before implementation. These results
describe a local implementation candidate. Cargo package metadata remains 0.1.0;
the documentation target 0.3.0 is not a published version or release tag.

Local toolchain: Rust 1.93.0 (`stable`), Windows x86_64 MSVC. The repository's
toolchain pin and dependency lockfile were retained.

## Decisions for the seven workstreams

All seven workstreams have production implementation and semantic regressions.
The measurements below distinguish real baseline comparisons from optimized-only
observations. External platform and long-duration gates remain separate.

| Item | Implemented behavior | Verification and limits |
| --- | --- | --- |
| OPT 1 | Indexed file/directory collision validation replaces repeated scans of previous paths | Duplicate, alias, traversal and parent/child order regressions; identical synthetic validation/import fixtures at 1k, 10k and 100k files |
| OPT 2 | Canonical/alias controller lookup, changed-row reduction, structural reconciliation and ordered telemetry coalescing | Removal/re-add/alias and ordering regressions; optimized-only event/publication timings, without a reconstructed reducer baseline |
| OPT 3 | Owner wakes on actual work and explicit deadlines; unchanged telemetry does not publish a full native snapshot | Idle-wake and boundary-notification regressions; optimized-only live CPU/RSS and end-to-end attach/stop observations |
| OPT 4 | Explicit persistence intents and narrow durable scalar acknowledgements preserve canonical child records in place | SQL rollback/retry, credential and child-preservation tests; 100k-file scalar ACK instrumentation with FULL synchronization |
| OPT 5 | Loaded, compact-owner and summary-mirror file models; bounded background path hydration and IPC paging | Deferred-save guards, exact totals, stale-generation and paging regressions; direct representation accounting and phased process sampling |
| OPT 6 | Bounded asynchronous writer, sequence watermarks, segment rotation, manifest validation and complete export | Slow/failing writer, overflow, timeout, rotation/reopen/tamper and explicit shutdown-barrier regressions; baseline journal comparison |
| OPT 7 | One persistent daemon owns the engine/database; GUI and TUI attach through a separate trusted local channel | Actual copied-executable lifecycle test with real loopback transfer across frontend exit/reconnect; Windows pipe EOF regression; native protocol and mutation tests |

## Ownership and compatibility

GUI/TUI no longer create a second engine or write profile SQLite independently.
Closing either frontend leaves the daemon running. Explicit owner stop drains
engine events, checkpoints actual totals and completes the journal barrier before
releasing profile ownership. The Stop CLI confirms acceptance and lock release;
it cannot infer every durability result without the background process's exit
status. Tests independently reopen SQLite and inspect journal completeness.

The native protocol is separate from external agent access. It uses bounded
authenticated frames, current-user endpoint protection, owner-instance and
revision fences, bounded queues and independent expiring detail leases. Public
agent access remains restricted and disabled by default. An incompatible legacy
owner is reported; it is never killed or replaced automatically.

Native mutations retain their engine/persistence acknowledgement boundaries.
An action queued by a UI is not yet accepted by the owner, and owner acceptance
is not durable completion. Reconnect invalidates stale owner-bound jobs and
metadata. After closing its window or terminal, a frontend allows up to eight
seconds for its submitted-action queue to receive owner replies, without stopping
the daemon. Audit workers cancel/drain first under a separate eight-second bound;
audit writers then receive a five-second completion deadline. Errors from every
phase propagate. A blocked underlying call can outlive a reported timeout.

Save View changes persisted UI fields only. Existing payload roots are pinned
before a global default changes; invalid relative roots or known unsupported
network settings fail before writing preferences. Restored registration and high
priorities use bounded admission and matching acknowledgements. Removal waits
for actual engine detachment and retries durable deletion; data recycling uses
single-use owner-issued confirmation plans bound to current filesystem identity.

See [native frontend operation](../../native-frontends.md) and
[journal operation](journal-operation.md) for startup, shutdown, migration,
authentication, bounds and failure semantics.

## Validation

The completed Windows matrix is recorded below. Counts overlap across feature
configurations and must not be added as a unique-test total.

| Configuration | Passed | Ignored |
| --- | ---: | ---: |
| All features / all targets, application and vendored tracker | 729 | 12 |
| Core only / all targets, application and vendored tracker | 597 | 10 |
| TUI only / all targets, application and vendored tracker | 655 | 11 |

The all-feature total includes two copied-executable native lifecycle tests.
One keeps a real TCP seed connection alive across GUI/TUI attach, exit and
reconnect, verifies three 16 KiB payload blocks, checks unchanged owner and engine
peer identities, rejects a second owner, then independently reopens SQLite after
explicit stop and verifies actual upload counters and unchanged payload. The
second verifies frontend process and stdout/stderr EOF completion while its
Windows background owner remains alive. All initiated test traffic is loopback;
listener defaults are not operating-system network isolation.

Standalone audit tests also use a real loopback HTTP server. A reply released
only after cancellation and display-receiver drop is admitted independently and
verified as exactly one JSONL record. Retained journal clones, manifest rename
failure and initial-open failure have explicit real filesystem shutdown tests.

The feature configurations use these commands, with `FLAGS` set to
`--all-features`, `--no-default-features`, or
`--no-default-features --features tui`:

```text
cargo +stable test --locked FLAGS --all-targets -p peerlume -p librqbit-tracker-comms
cargo +stable test --locked FLAGS --doc -p peerlume -p librqbit-tracker-comms
cargo +stable clippy --locked FLAGS --all-targets -p peerlume -p librqbit-tracker-comms -- -D warnings
```

Strict Clippy passed in all three configurations. Documentation test commands
also passed in all three; they found zero doctests, so they add no executed-test
count. [Validation metadata](measurements/validation-windows.json) preserves exact
commands, feature counts and SHA-256 identities of the locally retained raw logs.
Formatting, Python/PowerShell contracts, workflow syntax and repository language
checks also passed.
The 33 Python contracts cover launchers, packages and resource samplers. The
headless dependency check rejects GUI toolkits in the TUI normal/build graph.

The GUI, TUI, daemon, control adapter, MCP adapter and workload example built
successfully in Release with:

```text
cargo +stable build --locked --all-features --release --target x86_64-pc-windows-msvc --bins --example optimization-workload
```

[Build identities](measurements/release-build-identities.json) retain their hashes
and the separately executed Release measurement-test binary's hash. All 285
recorded source/build/tool inputs match before compilation, after measurements
and the final current-source verification. Executable hashes were independently
rechecked against the built files. Outputs use
`target/x86_64-pc-windows-msvc/release/`; the native guide explains the sibling
daemon requirement.

Ignored cases include explicit environment-dependent integrations and manual
Release measurement workloads. They are not inferred to have passed from the
ordinary matrix. The controller and 100k-file SQL measurement cases are executed
separately in Release; Docker and real ENOSPC remain external checks.

## Measurement evidence

Raw reports, exact workload identities, successful samples, timeouts and
comparison limits are retained in [measurements](measurements/README.md) and its
[comparison](measurements/comparison.md).

The first integrated Windows Release capture produced these rounded synthetic
results. Repeated journal controls and raw values remain in the comparison;
these observations are not universal application speedup claims.

| Workload | Preserved baseline | Integrated candidate |
| --- | ---: | ---: |
| Validate 10,000 relative paths | 5,604.98 ms | 5.80 ms |
| Parse/import 10,000-file metainfo | 5,542.27 ms | 13.61 ms |
| Validate 100,000 relative paths | Timeout at 15 s | 59.06 ms |
| Parse/import 100,000-file metainfo | Timeout at 15 s | 141.09 ms |
| Journal live-page p95, fixed contention fixture | 59.7 us | 0.8 us |
| Journal complete export, 128 observations | 7.76 ms | 33.94 ms |

The first complete-export duration regressed. The candidate fences the writer
and validates complete manifest/sequence history; the measurements do not
attribute the whole difference to any single component. Async admission improves
the producer/live-view boundary and is not a claim that complete durable export
is faster. Three additional alternating pairs confirmed baseline exports at
7.907-8.226 ms and candidate exports at 27.186-27.494 ms. The combined producer
plus export duration also regressed. Initial and repeated results are retained.

For 100,000 files, accounted dynamic buffers in loaded / compact-owner /
summary-mirror phases were 11,533,787 / 1,625,000 / 0 bytes. Their sampled process
RSS peaks were 18,239,488 / 6,520,832 / 4,890,624 bytes. Each held phase has 20
samples at a requested 10 ms interval; observed maximum gaps were about 11.1 ms.
This is a synthetic model
process with three successive representations, not a full engine/client RAM
comparison. Zero per-file buffer capacity still has a nonzero inline object and
process working set.

The empty disposable owner observation covered about two seconds after warmup.
Its CPU counter delta was zero at the available counter granularity, which does
not prove zero CPU usage. Sampled RSS peaked at 12,455,936 bytes across 20 samples
at a requested 100 ms interval; the maximum gap was 101.056 ms and the unsampled
tail 91.213 ms. Attach took about
181 ms including frontend startup, and explicit stop about 1,150 ms including
CLI startup and profile-lock waiting. The owner identity was unchanged, actual
daemon exit was successful, and its journal manifest was clean. These are
optimized-only observations, not a measured before/after idle CPU ratio.

The optimized controller microbenchmark applied 20,000 direct synthetic events
to 100 / 1,000 / 5,000 / 10,000 / 100,000 registered torrents in 9.202 / 10.359 /
10.619 / 11.892 / 16.698 ms respectively. The unchanged publication fast path took
about 0.052 ms for 20,000 calls at each size. These are offline, hot-loop timings;
they do not establish full transport-drain or end-to-end engine acknowledgement
latency at those scales. That broader part of the plan's ACK measurement remains
unverified.

For a real 100,000-file SQLite record, 20 scalar acknowledgements under FULL
synchronization took 28.1176 ms in total. Instrumentation observed 100 statements,
1,860 SQLite VM steps, zero fullscan steps, zero canonical child read/write
authorizations and 40 scalar row changes. All 15 aborting child-write guards
remained intact. Setup and post-verification were excluded; instrumentation was
included. These are SQLite work counters, not physical I/O counters, and there
is no matched pre-change ACK timing or speedup claim.

The baseline executable was frozen before source edits and its recorded compile
inputs match the baseline Git blobs. Both 100k-file baseline validation/import
children hit their explicit 15-second limits. Their full duration and exact
speedup are unknown. The reports preserve those failures.

Cold-model accounting compares three real representations in the integrated
build. A pre-change file-model process-RAM capture was not made, so that part of
the plan's comparative memory acceptance remains unfulfilled. Dynamic buffer
capacity excludes fixed objects, allocator overhead, SQLite and engine metadata.
RSS can retain freed allocations, and sampled values can miss shorter peaks.
The idle experiment is short and optimized-only. None of these observations
establishes a whole-client memory percentage, production WAN throughput, zero
idle CPU or absence of every leak.

## Remaining external gates

- Linux compilation, runtime, packaging and resource measurements need actual
  Linux execution. Windows core/TUI feature tests do not constitute Linux tests.
- Copied executables use the actual GUI/TUI native backend through
  `--attach-check`; they do not automate a visible GUI window or interactive TUI.
- No new continuous four-hour or 24-hour soak was started. These changes do not
  close an earlier or future long-duration gate.
- Docker/real ENOSPC and platform service/installer checks remain their existing
  explicit environment-dependent gates. Ignored tests are reported separately.
- This evidence was captured before commit/push. Release publication, tagging
  and package-version changes remain separate actions. Verification used only
  disposable synthetic and loopback fixtures; the user's running application
  and profile were not restarted or migrated.

The implementation is ready for review with the recorded Windows evidence and
the named external gates. Release acceptance still requires those platform and
duration checks where applicable.
