# Release launch and performance workload

## Everyday local launch

Repository launchers build and run **Release** by default. They require the
repository's normal Rust toolchain and Python 3.10 or newer. They do not install
packages, move payloads, rewrite settings or bypass the existing profile lock.

Windows (PowerShell 7.3 or newer):

```powershell
./scripts/run-client.ps1
./scripts/run-client.ps1 -Tui
./scripts/run-client.ps1 -Dev
./scripts/run-client.ps1 -ClientArguments @('--profile', 'C:/absolute/synthetic-profile')
```

Linux (Bash):

```bash
bash scripts/run-client.sh
bash scripts/run-client.sh --tui
bash scripts/run-client.sh --dev
bash scripts/run-client.sh -- --profile /absolute/synthetic-profile
```

In PowerShell, `-Development` aliases `-Dev`; `-NoBuild` selects an existing
executable and `-PrintCommand` emits a JSON plan without compiling or launching.
Use `-ClientArguments @('...', '...')` for literal application arguments, including
empty values or application `--help`/`--dev`. The wrapper inserts the native
separator itself because PowerShell consumes a script invocation's `--` marker.

In Bash or when invoking `python scripts/run_client.py` directly, use `--dev`
(`--development`), `--no-build` and `--print-command`. Arguments after `--` are
passed unchanged to the frontend. Paths containing spaces remain single arguments.
`CARGO_TARGET_DIR` and `CARGO_BUILD_TARGET` are honored. Cross-target binaries must
be runnable on the host to launch successfully.

The build respects the checked-in Rust toolchain and normal Cargo environment;
the launcher does not silently replace a pinned toolchain. A running instance
keeps ownership of its profile. Exit that instance normally before replacing its
executable or deliberately run a separate disposable profile. Packaged installed
applications can continue to use their native shortcut or executable directly.

## Synthetic workload

Run the identical workload in development and release profiles, using new report
paths for every run:

```powershell
cargo run --locked --all-features --example perf-workload -- --output NEW_DEV_REPORT.json --source-label SOURCE_LABEL
cargo run --release --locked --all-features --example perf-workload -- --output NEW_RELEASE_REPORT.json --source-label SOURCE_LABEL
```

The same Cargo commands work in Bash. Add the same explicit `--target` triple to
both commands if reusing a target-specific build cache. Avoid concurrently running
benchmarks, compilers, checks or other heavy disk work when comparing timings.

Inputs are deterministic: 1, 10 and 100 paused synthetic torrents with 100 files
each; repeated name-sort views; unchanged and changed transactions in a temporary
file-backed SQLite database; construction and cached filtering of 100,000 files.
The default is 200 view repetitions and up to 20 repetitions per checkpoint case.
Use `--repetitions 1..10000` to change both runs consistently. Model/setup time is
outside the view and checkpoint samples; large-model construction is separate.

The workload never creates a transfer session or contacts a tracker/peer. Its
temporary database is deleted after measurement. Reports use create-new writes,
so an earlier evidence file cannot be overwritten. A final `completed=true` is
written only after the workload validates final persisted synthetic counters.

JSON includes actual elapsed nanoseconds, p50/p95/p99 samples, repetitions, OS,
architecture, debug-assertion state, package version and compile-time SHA-256
digests of relevant source inputs. The human-readable source label is supplied
by the caller; input digests are the checkable identity. Preserve the command,
compiler version, machine and base/optimized source identity alongside reports.

The Rust report contains wall-clock microbenchmarks, not CPU-percent, RSS or heap
allocation measurements. Those fields are explicitly unavailable in that report.
The separate process sampler below supplies actual process CPU and sampled RSS.
Short samples can be noisy; compare repeated runs on the same machine and
distinguish warm-cache behavior from startup. Existing resource soaks cover
different workload and sampling semantics; these reports do not close the
24-hour release gate.

The original baseline invokes the stateless name-sort view API on each repetition.
The optimized frontend workload retains a `ViewCache`, as the actual frontends
now do. It verifies the same sorted authoritative identities, shared result
allocation and a single rebuild for unchanged inputs before reporting success.
The JSON `view_implementation` field identifies that optimized path. This is an
intentional caller/API optimization, not a claim that the old stateless call
has become allocation-free.

Recorded baseline source is `2c5cf1a`; the development and release reports contain
matching compiled input digests, verified against that commit's Git blobs, and
were collected with Rust 1.93.0 on
`x86_64-pc-windows-msvc`. Raw baseline evidence:

- [Development baseline](measurements/baseline-development.json)
- [Release baseline](measurements/baseline-release.json)

The user's existing interactive client remained running. Ambient machine load
was not controlled, and these individual runs should not be interpreted as
guaranteed production speedups.

## CPU and RSS of the synthetic workload process

The standard-library-only sampler starts exactly one compiled workload executable.
It does not start a compiler, attach to the running client, open a user profile or
include the sampler's own CPU/memory in the child measurements:

```powershell
python scripts/sample_perf_process.py --executable PATH_TO_PERF_WORKLOAD --workload-report NEW_WORKLOAD.json --report NEW_RESOURCES.json --source-label SOURCE_LABEL --repetitions 200 --sample-ms 10
```

Use the same compiled profile, workload inputs and repetition count for baseline
and optimized measurements. Preserve the baseline executable before rebuilding;
an executable SHA-256 and the child report's compiled-input digests identify what
was actually measured. The sampler validates the complete 1/10/100-torrent child
report before emitting `completed=true`. Existing evidence paths are rejected.

On Windows, an independent handle to the spawned process feeds
`GetProcessMemoryInfo` samples and final `GetProcessTimes` user/kernel CPU totals,
including CPU before the first successful RSS sample. On Linux, RSS/HWM samples
come from that child's `/proc/PID/status`; completed CPU totals use the
`resource.getrusage(RUSAGE_CHILDREN)` delta with one child only. Linux support is
implemented but must be distinguished from an actually executed Linux measurement.
The units follow the official [Win32 CPU-time documentation](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes),
[process-memory counters](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters)
and [Python resource API](https://docs.python.org/3/library/resource.html).

The report includes requested sampling interval, successful sample count, first
and last offsets, largest successful-sample gap and unsampled start/tail spans.
The default is 10 ms; scheduling and process-query setup can create larger gaps.
The sampled RSS peak can miss shorter spikes. An observed operating-system
high-water mark is reported separately through the last successful query; it is
not a heap profile or a claim of complete lifetime sampling. CPU percent is
average CPU time divided by process wall time, with one logical core as 100%.
Wall time includes process creation and resource-probe setup, so use matching
sampled runs when comparing end-to-end results.
Windows working set includes shared as well as private resident pages; it is
not private heap usage. See [Microsoft's working-set definition](https://learn.microsoft.com/en-us/windows/win32/procthread/process-working-set).

The preserved baseline was sampled on Windows using the same 200 repetitions:

- [Development workload](measurements/baseline-development-sampled-workload.json)
  and [resource report](measurements/baseline-development-resources.json)
- [Release workload](measurements/baseline-release-sampled-workload.json)
  and [resource report](measurements/baseline-release-resources.json)

All sampled baseline compiled-input hashes were verified against the original
baseline reports. Evidence stores only the executable/report file names, source
hashes, synthetic workload metadata and measured values. No private absolute
paths were collected in successful reports; no measurement values were redacted
or rewritten when copying them into release documentation. The existing client
and ambient activity remained outside the sampled child but can affect its
wall-clock timing through shared machine resources.
