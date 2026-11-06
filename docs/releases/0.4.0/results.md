# PeerLume 0.4.0 implementation and verification

Status: implementation candidate. Local matrices, final instrumented GUI tests
and native lifecycle checks passed. OS pixel inspection remains blocked by the
capture provider.

Recorded on 2026-11-06 (Europe/Berlin). This document accompanies the
[versioned release plan](release-plan.md), saved before implementation.

## Tested source and scope

The baseline is repository commit
`c949c91a6d3c1b7730d3faf2e396f184d5d45f03`. Verification is against the dirty
working-tree candidate, not a new commit or published release. The final source
capture contains 324 input files and aggregate SHA-256
`717ba2c7d5fe7fda706aadfe899f3595098d3a6350222ae0dc2a59a7dc3575a3`.
Before/after manifests match exactly for the final capture. The earlier full
matrix and candidate binaries used source capture
`18b8e8426d23bcaa12989b77800079d1079eac12c701623d8eb41fbd196d9873`.
The only later input changes were the LLVM 3 Python reader/contracts and two
child GUI test files. Production Rust remained byte-identical. See the
[final verification proof](measurements/final-verification-proof.json) and
[source inputs](measurements/source-inputs-final.json).

The plan target is 0.4.0; the Cargo package version remains 0.1.0. During the initial local implementation phase, no commit, push, package
publication or restart of the running user's client was performed.
Tests use disposable state and owners. The existing user's profile, downloads
and running GUI/daemon were left untouched.

## Implemented behavior

1. **Adaptive command bar.** Primary add/search/selected-target commands remain
   reachable at narrow widths and enlarged text. Overflow and More expose
   secondary storage, copy, cleanup and global commands with their prerequisites
   and confirmation scope. Agent access uses a compact entry. Command-bar and
   context actions share real dispatch/validation paths.
2. **Configurable torrent table.** All eight columns support explicit widths,
   optional visibility, ordering and restoring defaults; Name stays available
   and fills unused width. Header sorting covers the column metrics with defined
   ties/unknown values. The pinned header shares horizontal geometry with the
   virtual body, including the scrollbar gutter. UI-only persistence retains
   unrelated saved preferences and separately unsaved drafts.
3. **Identity-based multiple selection.** Ctrl/Shift clicking uses
   canonical torrent IDs across sorting, filtering and updates. Visible/hidden
   selection counts make bulk scope explicit. Batch actions freeze their targets
   and owner/revision assumptions; stale requests are rejected rather than
   redirected to a neighboring row. Removal retains its confirmation boundary.
4. **Selectable file hierarchy.** The file area renders a virtualized hierarchy
   with stable folder identities, parent context during search, expansion and
   folder selection and visible-file priority controls. Detail loading and metadata generations
   fence index-based actions. Native protocol 2 rejects stale file-scope requests
   before mutation; transfer, path and payload protections are retained.
5. **Visible background-owner control.** The footer reports authoritative totals
   and connection state, including reconnecting, stop-requested and stopped
   states. Mutation controls reject disconnected/stale owners. Closing the
   frontend leaves its background owner running. Explicit Stop requires
   confirmation of the captured owner; stopping does not automatically launch a
   replacement owner.
6. **Bounded operation history.** Distinct requested operations retain independent
   IDs, targets, progress and errors, with at most 256 rows and 512 KiB of reserved
   display storage. Queue/owner acceptance remains unconfirmed. Pause/Resume need
   matching engine feedback, owner and command revision plus saved-state proof.
   Recheck needs actual integrity verification; checking start or 100% progress
   alone is insufficient. Removal needs a later authoritative owner snapshot
   after the accepted command. Transport ambiguity, replacement, superseding
   revision or missing proof stays Needs review. Import and navigation errors
   survive later transient status messages. Focus, sanitized Copy error, Dismiss
   finished and explicit Dismiss unconfirmed are safe display actions; they do
   not retry or cancel work or turn an uncertain result into success.
7. **Consistent appearance.** System/Light/Dark, Compact/Comfortable and bounded
   75-200% text/zoom settings apply through UI-only persistence. Absolute zoom
   avoids multiplying native DPI on repeated frames. Both theme styles receive
   density changes, with measurement invalidation only when relevant settings,
   DPI or active theme change. Selection spans the row. Table status cells render
   markers together with readable state text. Palette tests check the specified foreground/background
   pairs rather than claiming full application accessibility certification.

The external agent/CLI/MCP protocol remains version 1 and restricted. The trusted
native frontend protocol is version 2 so an older owner cannot silently discard
the new presentation settings or file-generation proofs. Upgrading a normal
profile requires its owner's explicit normal shutdown; this task did not stop
the already-running user instance.

## Executed checks

Counts for focused checks overlap the full matrix and must not be added together.
The matrix runs use fresh configuration/data environment directories and two
test threads. No ordinary profile is selected by these test commands.

| Check | Current verified result |
| --- | --- |
| Windows all-features application/tracker, all targets | 848 passed, 0 failed, 13 ignored across 19 target reports; library subset 777 passed, 7 ignored |
| Windows core-only application/tracker, all targets | 659 passed, 0 failed, 11 ignored across 16 target reports; library subset 590 passed, 5 ignored |
| Windows TUI-only application/tracker, all targets | 717 passed, 0 failed, 12 ignored across 17 target reports; library subset 648 passed, 6 ignored |
| Focused native generation/revision guards | 35 passed; followed by full all-features pass |
| Focused controller/native evidence | 4 passed; followed by full all-features pass |
| Final App rendering/action tests | 132 passed, 1 ignored; actual App binding subset 19 passed |
| Fixture writer safety/schema regression | 1 passed; rich profile preparation separately invoked explicitly and passed |
| Python packaging/launch/measurement/coverage/candidate contracts | 54 passed |
| Standalone selective-coverage wrapper | 2 Windows argument/query passthrough tests passed |
| Formatting | Passed |
| Strict Clippy, all features and TUI-only, all targets, warnings denied | Both passed |
| Headless dependency check | Passed |
| Documentation tests, all features/core/TUI | All three passed; 0 doctests executed |
| Focused vendored engine checks | 2 passed, 37 filtered out |
| Actionlint, repository language and diff whitespace checks | Passed |
| Candidate Release GUI/daemon alias build and disposable rich profile | Passed; production entry points, 80 paused torrents, first with 150 nested files |
| Native owner continuity/stop on disposable profile | Passed; normal frontend close preserved same owner and 80 rows; explicit scoped stop released ownership without automatic restart |
| Native pixel inspection | Blocked by capture-provider timeouts; no pixels were inspected |
| Final instrumented Windows GUI source coverage | 788 passed, 0 failed, 7 ignored; 3,684/3,988 lines and 390/406 functions in the eleven new production modules |

The all-features command was:

```text
cargo +stable test --locked --all-features --all-targets -p peerlume -p librqbit-tracker-comms -- --test-threads=2
```

The all-target matrices precede eleven additional GUI child tests; the final
instrumented library pass includes all 788 current tests and strict all-target
Clippy passed after those additions. No production code changed between these
checks. Ignored tests are not represented as passed. Rich fixture preparation
was subsequently invoked explicitly and succeeded; it writes synthetic state
without launching a transfer.

## Coverage and native candidate evidence

Use the [coverage procedure](gui-coverage.md) and [candidate procedure](native-qa.md).
The coverage scope requires positive covered line and function counts for all
eleven new production modules. Existing app/navigation/native/settings/view
context is reported separately and excluded from the new-module aggregate.
New-module tests reside in child files so their bodies do not inflate those
production-file totals. Real LLVM JSON/LCOV provide the counts and uncovered
locations; test counts are not substituted for coverage.

The first instrumented library run used Rust 1.93.0/LLVM 21.1.8 and passed 777
tests with 7 ignored in 136.91 seconds. Its eleven new production files measured
3,521/3,988 covered lines (88.28986960882648%) and 381/406 covered functions
(93.8423645320197%). These are actual LLVM summary counts. The initial pipeline
correctly rejected an unsupported JSON export version; the actual tool emits
schema 3.0.1. After checking the primary LLVM exporter and adding a contract test,
the Python reader was extended to schemas 2/3 while retaining every count,
required-source and positive-execution check. The original failed capture,
executable, profiles and raw exports are preserved. Reprocessing the same exports
did not rerun the successful tests or change their source.

The first review identified unexecuted App footer/dialog bindings and additional
tracker disconnect/receipt-boundary cases. Eleven meaningful tests were added in
child test files; no production Rust change was made for those additions. The
final capture passed 788 tests with 7 ignored in 141.31 seconds. Every compile,
test, profile merge, export and validation command succeeded. All seventeen
required/context source hashes remained unchanged. The final instrumented test
executable SHA-256 is
`b2bf39fa4871840cae57bfed83d78ffeeaa90c79a190efc2adde1d20a6a7aece`.

| New production source | Covered lines | Covered functions |
| --- | ---: | ---: |
| `appearance.rs` | 192/192 (100.00%) | 23/23 (100.00%) |
| `ui_operations.rs` | 697/748 (93.18%) | 82/83 (98.80%) |
| `table_view.rs` | 178/181 (98.34%) | 30/30 (100.00%) |
| `torrent_selection.rs` | 135/135 (100.00%) | 29/29 (100.00%) |
| `file_selection/hierarchy.rs` | 353/359 (98.33%) | 41/46 (89.13%) |
| `file_selection/metadata.rs` | 109/109 (100.00%) | 20/20 (100.00%) |
| `app/file_tree.rs` | 350/386 (90.67%) | 27/28 (96.43%) |
| `app/torrent_table.rs` | 560/572 (97.90%) | 43/43 (100.00%) |
| `app/desktop_controls.rs` | 363/367 (98.91%) | 27/27 (100.00%) |
| `app/operation_tracking.rs` | 283/363 (77.96%) | 20/22 (90.91%) |
| `app/desktop_integration.rs` | 464/576 (80.56%) | 48/55 (87.27%) |
| **Weighted total** | **3,684/3,988 (92.37713139418254%)** | **390/406 (96.05911330049261%)** |

[Final source counts](measurements/final-scoped-summary.json) include separate
existing app/navigation/native/settings/view context. Its coverage ranges from
39.31% lines in the large existing `app.rs` to 95.77% in `torrent_view.rs`; these
files are excluded from the new-module aggregate. Counts are line/function
coverage, not branch coverage or complete application certification.

Remaining uncovered new code includes native picker/storage dispatch paths,
context-menu bindings and some display actions, category mapping, the
Resume-success bridge branch and defensive job trimming. Safety coverage was
expanded for disconnection/stop states, first-snapshot owner mismatch,
replacement command revision, unverified recheck completion and full-history
admission. Widget/model tests do not establish every OS interaction. The
[LCOV location review](measurements/final-uncovered-review.json) preserves the
remaining paths; unique zero-DA lines and generic instantiations are location
aids, not substitutes for LLVM summary denominators.

The Windows GUI coverage CI job is prepared with Rust 1.93.0, matching LLVM tools
and cargo-llvm-cov 0.9.1. Its structural/execution gate rejects missing, malformed,
empty or unexercised required files. A new numeric Windows CI baseline remains
`not_established` until that exact job is measured and reviewed. The local
selective wrapper and CI have different orchestration/coverage cfg flags, so a
local result is not silently adopted as the CI regression threshold.

The candidate GUI SHA-256 is
`4dd8093b342244815c3ff01ade2887e8fe7799d68a8ebc1cf9b2fefe03987844`;
the daemon SHA-256 is
`2982a96b1fae8a9265b784083462a9f74d2c8401ddf2fd4f683a8ae82f739bb8`.
Aliases compiled the production entry points from source capture `18b8...` and
were copied into a new GUID directory rather than replacing running binaries.

The first candidate GUI closed normally with Alt+F4. A later native attach still
reported the same owner
`2d8a50facbb3723767e1428ec9a5da3f31c12c9b066bdbbcc29700be29fa14cd`
and 80 saved torrents. A later properly attached GUI used owner
`4b7db40ec3c5622b050a726a98b1bedaee393b8846a908d2826223e10da1c354`;
the profile-scoped daemon stop returned exit 0 and released the profile lock.
The GUI stayed responsive after stop and no daemon automatically restarted. All
candidate owners/windows were closed. The user's original GUI/daemon PIDs,
executable paths and normalized UTC start ticks remained unchanged.

Actual window pixel inspection was blocked after refresh by both
`FrameArrived timed out: timed out waiting on channel` and
`window capture timed out: timed out waiting on channel`. Text-only native
window access and authenticated owner probes succeeded. No screenshots or pixel
claims are substituted for that failed inspection. Wide/narrow/theme/zoom
behavior therefore has automated egui evidence, with OS pixel inspection still
unverified. A launch racing with explicit owner stop was correctly rejected
without recording a replacement owner.

The [native lifecycle record](measurements/native-lifecycle.json) retains those
identities/outcomes and the inspection blocker with its machine-specific
candidate directory redacted.

## Verification limits and inherited CI state

Some accepted settings/storage actions do not expose correlated final engine and
durability evidence. Their history entries require review instead of a green
completion. Explicit dismissal forgets only that display record after the user
inspects current state. It does not establish a result or send another command.
Stopped/lock-release evidence alone is not presented as a blanket database or
journal durability assertion.

Headless egui tests cover real rendering/input and App command handlers; native
pickers, actual OS window behavior and Linux desktop/file-manager integration
remain separate checks. The local core/TUI feature matrices execute on Windows.
They do not establish a Linux runtime/window result.

The baseline commit already had these verified GitHub results:

- [Tests](https://github.com/javaquasar/peerlume/actions/runs/42034844007)
  succeeded.
- [Coverage](https://github.com/javaquasar/peerlume/actions/runs/42034843625)
  failed its existing Linux line gate: 83.5690168154314% against 86.879%.
  Instrumented tests/soaks had passed; the existing numeric baseline is retained.
- [Reliability](https://github.com/javaquasar/peerlume/actions/runs/42034843703)
  failed native-window startup because `peerlume-daemon` was missing beside the
  frontend. The X11 build now includes the required daemon. A later Linux native
  run is still needed to verify the updated workflow.
- [Build packages](https://github.com/javaquasar/peerlume/actions/runs/42034843468)
  failed the Windows desktop library test `a_timed_out_barrier_is_not_fabricated_success_and_can_be_retried`
  with `released journal barrier did not complete: Journal flush barrier timed out`.
  Other package jobs succeeded. This is recorded as an inherited failure; no
  production timeout, assertion or Linux baseline was weakened for this UI work.

These historical runs did not verify the local candidate. The initial local
implementation phase did not start a GitHub workflow, long swarm run or publication.
The separately authorized push verification is recorded below.


## Authorized GitHub push verification follow-up

The complete candidate was committed and pushed to `main` as
`1adfb71803be0e30c3446c3303cd83e333521f28` on 2026-11-06. The following
first-attempt results were read from GitHub jobs and their full failure logs:

- [Tests](https://github.com/javaquasar/peerlume/actions/runs/42250297315):
  Linux core/TUI, Docker, formatting and strict Clippy passed. Windows reported
  787 passed, one failed and seven ignored. The inherited journal retry test
  failed with `released journal barrier did not complete: Journal flush barrier
  timed out`.
- [Reliability](https://github.com/javaquasar/peerlume/actions/runs/42250297239):
  seven jobs passed. X11 startup failed because the copied test installation
  omitted the required sibling daemon. The downloaded application log states
  `peerlume-daemon is missing beside the frontend; build/install the complete
  application package`.
- [Coverage](https://github.com/javaquasar/peerlume/actions/runs/42250297346):
  Windows GUI passed with 788 tests and seven ignored. The eleven new modules
  measured 3,684/3,988 lines (92.38%) and 390/406 functions (96.06%), matching the
  local measurements. All Linux instrumented workloads passed, but its measured
  84.6611363280639% line coverage remains below the unchanged 86.879% gate. This
  improves on the inherited 83.5690168154314% result; the shortfall remains open.

The follow-up corrects the copied native fixture rather than changing application
permissions or bypassing UI input. It supplies all three executables, navigates
the actual More menu and agent window, and explicitly stops only its disposable
profile owner before expecting a new owner identity. Secondary-menu lookup uses
bounded real wheel input when its target is outside the popup viewport. Closing
the GUI still must
preserve that owner. Exact Done OCR, independent payload hash verification,
positive damaged-piece refetch and normal reporting mode remain required.

The journal regression test retains its injected 30 ms failure and five-second
recovery budget. Recovery now sends one fence under that existing overall budget
instead of creating a backlog of serial fsync requests every 30 ms. The shared
internal deadline helper retains the production configured timeout. The test
also proves a queued recovery cannot succeed while the writer gate is closed,
then requires an acknowledged snapshot and durable sequence 1. All 13 focused
journal tests and strict core Clippy passed locally. The final Python contract
suite passed all 70 tests, including 16 new native-package/menu/lifecycle cases;
formatting, actionlint and the repository language check also passed.

This section records the first push and the rationale for the follow-up. It does
not represent the follow-up commit's GitHub jobs as passed before they complete.
The user's existing client and profile remain untouched; no release was published.
