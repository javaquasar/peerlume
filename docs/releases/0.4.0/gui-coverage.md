# Windows GUI coverage

The independent `windows-gui-coverage` job in `coverage.yml` instruments the
application library with GUI and TUI features. Its tests include actual egui
layout/input rendering and the application's action/receipt regression tests.
They use synthetic state and disposable owners; the coverage job does not attach
to an existing user profile or require a real desktop window.

This path preserves the Linux core/TUI job and its measured numeric baseline.
Neither a passing test count nor the existence of a coverage workflow establishes
new GUI coverage. The Windows job exports actual LLVM JSON, LCOV and HTML, then
requires every source listed in `config/windows-gui-coverage-scope.json` to have
nonempty line and function measurements, with positive executed line and function
counts in every required new production file. Context files can honestly show
zero coverage. A missing module, duplicate module,
empty denominator, inconsistent percentage or malformed report fails validation.
Full reports retain the existing app/settings/frontend context as well as the
required new-module scope. Inspect the HTML/LCOV for uncovered paths; the scoped
summary reports counts and weighted percentages, not an average of percentages.

The tool versions are Rust 1.93.0 with its matching `llvm-tools-preview` and
`cargo-llvm-cov` 0.9.1. The job runs:

```powershell
python -m unittest discover -s scripts -p '*gui_coverage.py' -v
python scripts/check_gui_coverage.py --preflight target/coverage-gui-build --minimum-free-gib 8
cargo +1.93.0 llvm-cov clean --workspace
cargo +1.93.0 llvm-cov test --locked --all-features --lib -p peerlume --no-report -- --test-threads=2
cargo +1.93.0 llvm-cov report --json --output-path coverage/windows-gui/llvm.json
cargo +1.93.0 llvm-cov report --lcov --output-path coverage/windows-gui/lcov.info
cargo +1.93.0 llvm-cov report --html --output-dir coverage/windows-gui/html
python scripts/check_gui_coverage.py --output coverage/windows-gui/scoped-summary.json
```

The workflow sets `CARGO_TARGET_DIR=target/coverage-gui-build`, incremental builds
off, two build jobs and development/test debug information off to reduce disk
usage. LLVM's source coverage instrumentation remains enabled by the tool. Its
build cache is separate from normal binaries; it never overwrites or launches a
running Release client. The disk preflight fails before creating a build directory
when less than 8 GiB is free on that volume. This is an admission budget, not a
guarantee that every future dependency graph fits in 8 GiB.

If `config/windows-gui-coverage-baseline.json` exists, the job additionally checks
both aggregate and individual-file line/function percentages against that exact
scope. Establish that file only from an actual reviewed instrumented report; do
not fill it from estimates or lower the Linux baseline. Before a Windows numeric
baseline is established, the summary explicitly says `not_established`; only
schema, completeness and source admission are gated. A later measured baseline
must retain every required file and use `schema: 1`, scope `windows-gui-0.4.0`,
`totals: {lines: minimum_percent, functions: minimum_percent}` and a `files`
mapping from each required path to the same two minimum percentages.

The validator supports LLVM export schemas 2 and 3. The installed LLVM 21.1.8
exports `version: 3.0.1`, as its
[primary exporter source](https://github.com/llvm/llvm-project/blob/llvmorg-21.1.8/llvm/tools/llvm-cov/CoverageExporterJson.cpp)
specifies. Its per-file line/function count, covered and percentage fields retain
the checked shape; additional branch/MC/DC fields do not become coverage claims.
Unknown future major versions remain rejected. A first local capture rejected
schema 3 before this compatibility correction; preserve that failed capture and
reprocess its existing profiles/exports without pretending the first run passed.

Actual numeric captures and any local tool/disk blocker belong in the release
results. Headless egui coverage does not establish OS window behavior, rendering
on Linux, screen-reader accessibility, branch coverage or long-running engine
reliability. Those checks remain separate from this report.

## Local capture with the existing dependency cache

When a separate instrumented dependency build does not fit, the local driver can
reuse `target/debug` and instrument only the `peerlume` library through Cargo's
`RUSTC_WORKSPACE_WRAPPER`. It compiles the tiny standalone wrapper, checks literal
argument/query passthrough with its tests, builds the library test executable,
then runs that exact executable and merges its real LLVM profiles:

```powershell
python scripts/measure_gui_coverage.py --output ABSOLUTE_NEW_EVIDENCE_DIRECTORY
```

The wrapper adds `-C instrument-coverage` only to crate `peerlume`, preserving
original OS arguments. Compiler queries, build scripts, dependencies and other
binaries pass through without instrumentation. Cargo still rebuilds the library
with a distinct wrapper fingerprint; it reuses compatible dependency artifacts.
The driver requires the installed Rust 1.93.0 and matching LLVM tools, two GiB of
free space before starting, a new evidence directory and positive execution
deadlines. It does not remove old artifacts, install a toolchain or change a
profile. A missing matching tool or insufficient space is a reported blocker.

`capture.json` records commands, exit codes, tool versions, library executable and
source hashes. The required sources must stay unchanged throughout the capture.
`llvm.json`, `lcov.info` and `scoped-summary.json` come from the executed library
test binary. Failed tests or missing profiles fail the capture, retaining its
logs. Existing evidence directories are refused without modifying them. This
local report uses the same per-file summary schema as the CI validator; that
does not establish identical numeric denominators across different build/test
orchestration. Platform, flags and test sets must be stated when comparing
captures. Existing app/native/settings/view context files are reported separately
and excluded from the aggregate of the eleven new production modules.

The pinned coverage tool additionally defines `cfg(coverage)`; the selective
local wrapper only adds the instrumentation flag. Stable 1.93 does not need the
old unstable Windows codegen workaround. These choices can affect generic
instantiations or conditional paths in a future source/dependency graph.
Establish the Windows CI numeric baseline only from the actual reviewed CI job,
not by assuming a local differently orchestrated capture is equivalent. Until
that measurement exists, `not_established` is the honest gate status even when
all required local module counts are available.

Both paths isolate test configuration/data locations: the local driver overrides
`LOCALAPPDATA`, `APPDATA`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME`
with new directories inside that capture's `test-environment`; the CI test step
uses its fresh runner temporary directory. This isolation is applied by the tool
itself rather than relying on the operator to remember profile environment flags.
