# Synthetic measurement evidence for 0.3.0

Baseline: `3d8242fd0e34347e287e693f224de9873ff97e4f`. The preserved Windows
x86_64 Release executable was built before optimization source edits. Its four
embedded source-input SHA-256 values exactly match the Git blobs at that commit.
Its executable SHA-256 is retained in `baseline-release.json`.

The baseline runs validation and parsing for the same synthetic 1k, 10k and 100k
relative paths. A 15-second limit applies independently to each child. Both 100k
baseline cases timed out; their reports deliberately retain `completed=false`
and the exact timeout rather than an estimated total. Journal contention uses
128 observations with 512 peers each, 1,000 concurrent live-page reads and a
complete-history export.

Reproduce with a new report path:

```text
cargo +stable build --locked --all-features --release --target x86_64-pc-windows-msvc --bins --example optimization-workload
python scripts/measure_optimizations.py --executable target/x86_64-pc-windows-msvc/release/examples/optimization-workload.exe --source-label optimized --report NEW-COMPARISON.json
python scripts/measure_cold_models.py --executable target/x86_64-pc-windows-msvc/release/examples/optimization-workload.exe --source-label optimized --report NEW-MEMORY.json
python scripts/measure_idle_owner.py --daemon target/x86_64-pc-windows-msvc/release/peerlume-daemon.exe --frontend target/x86_64-pc-windows-msvc/release/peerlume.exe --source-label optimized --report NEW-IDLE.json
cargo +stable test --locked --all-features --release --target x86_64-pc-windows-msvc --lib control::tests::release_indexed_controller_measurement -- --ignored --exact --nocapture --test-threads=1
cargo +stable test --locked --all-features --release --target x86_64-pc-windows-msvc --lib state::optimization_tests::measure_scalar_ack_100k -- --ignored --exact --nocapture --test-threads=1
python scripts/capture_source_identity.py --repository . --base-commit 3d8242fd0e34347e287e693f224de9873ff97e4f --candidate-label optimized-worktree --report NEW-SOURCE.json
```

The memory runner holds three phases of the same 1k/10k/100k-file process while
sampling: loaded paths/tree/view, compact cold owner, then summary-only mirror.
Each phase retains the same authoritative counts and selected sizes. Reports
separate accounted dynamic buffers from actual sampled RSS and process-lifetime
high-water memory. Accounted buffers exclude inline model bytes, fixed shared
cache allocations and allocator overhead. A summary's zero per-file buffer
capacity does not mean a zero-byte object. Earlier phases can determine a later
phase's process-lifetime high-water value; allocator retention can keep RSS high.

Measurements run synthetic children only, without user profiles, trackers or
downloads. Timings include ordinary machine load and cache noise. Samples can
miss shorter peaks. These results do not measure engine metadata/SQLite memory,
WAN throughput, a 24-hour run or universal leak absence. Linux measurement remains
a separate platform check until its raw reports are recorded.

Raw report paths use evidence filenames rather than private machine paths. No
numeric result, hash, timeout or failure is removed during this sanitization.
Repository JSON copies use LF line endings. Original captured files remain in
the local evidence directory; parsed JSON values were verified unchanged during
line-ending conversion before commit.

The idle runner copies the daemon/frontend into a disposable directory and uses
an empty new profile with discovery, extra trackers, UPnP, uTP and blocklist
updates disabled. It does not attach to a user's existing process. After one
second of warmup it measures a two-second live-process CPU delta and RSS samples.
Its native attach latency includes frontend process startup and handshake/snapshot
work, rather than isolating IPC latency. It requests normal owner shutdown,
checks the actual child exit code and inspects the clean journal manifest.
Listener binding remains the engine default; this is not OS network isolation.
This optimized-only observation cannot establish a before/after idle CPU ratio.

[comparison.md](comparison.md) records all measured values, including the export
regression and all three paired controls. [source-inputs.json](source-inputs.json)
and [source-inputs-after.json](source-inputs-after.json) bind the current candidate
to 285 source/build/assets/tool files. Both captures match. Exact application,
example and ignored-test binary hashes are in
[release-build-identities.json](release-build-identities.json). These captures
describe the worktree on the stated base, rather than a post-commit measurement.
