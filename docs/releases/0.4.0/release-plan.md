# PeerLume 0.4.0: coherent desktop controls and presentation

Plan revision: 1.1

Created: 2026-11-06 (Europe/Berlin)

Initial status: planned. This plan is saved before implementation source edits.
Implementation decisions, executed checks, measured coverage and remaining
platform limitations will be recorded in `results.md` in this directory.

Baseline: `c949c91a6d3c1b7730d3faf2e396f184d5d45f03`.

Target release: 0.4.0. This version identifies the implementation plan, not a
published package, tag or implicit Cargo version change.

## Objective and existing foundation

Implement all seven agreed GUI improvements: a compact command bar, configurable
tables, multiple selection, a real file hierarchy, visible background-client
control, an operation history, and a consistent appearance. This is production
behavior with tests, rather than a visual mockup or controls without actions.

The baseline already provides status/category filters, retained search/sorting,
virtual rows, a growing/resizable Name column, a vertical pane splitter, a framed
file area, aligned progress/size values, file reveal, contextual recheck/removal,
pause/error feedback and one persistent daemon shared by native frontends.
Retain these behaviors instead of implementing them a second time.

## Non-negotiable invariants

- Do not stop, restart, migrate, benchmark or rewrite the running user's client,
  profile or downloads. Use disposable synthetic profiles for tests and previews.
- Keep the daemon as the single engine/controller/database writer. Presentation
  changes must not bypass authenticated native IPC, CAS revisions, file-scope
  confirmation or engine/persistence acknowledgement boundaries.
- Keep the external agent/CLI/MCP protocol restricted and disabled by default.
  A new GUI control must not widen its permissions or reveal native-only data.
- Store selection/focus by canonical identity. Sorting, live updates, removal and
  filtering must not silently redirect an action to a neighboring torrent.
- Preserve real transfer totals, file selection/priorities, repair/completion
  evidence, SQLite FULL durability and all existing path/payload protections.
- Preserve virtualization, cold-model release, bounded queues and adaptive
  repaint. No RPC, file-system scan or durable write may block a GUI frame.
- New UI settings are additive and defaulted. Loading older or malformed layouts
  must normalize safely without rewriting unrelated preferences or payload roots.
- Never present queued/accepted work as completed. A lost connection or changed
  owner invalidates outstanding assumptions; do not replay mutations blindly.
- Keep repository prose and code comments in English. UI strings use the existing
  English product vocabulary. This task does not introduce localization.

## Shared data and integration contracts

Extend `UiSettings` with core-safe serialized table and appearance models. Keep
`name_gap_pixels` compatible with existing profiles; Name continues to consume
available width instead of becoming a permanently fixed-width column. Store
other explicit widths in logical UI points, with defensive limits. UI-only saves
must preserve separately applied or unsaved global preference drafts.

Use small focused modules for table geometry/interaction, identity selection,
file hierarchy, appearance and operation history. Root integration owns `app.rs`
and common settings/native-client contracts; parallel workstreams avoid whole-file
overwrites. Domain models must remain usable/testable without the GUI feature.

## UI 1: compact and adaptive command bar

Keep Add torrent, Add magnet, search and primary selected-target Pause/Resume
actions immediately accessible. Put storage relocation, verified copy, duplicate
cleanup and other infrequent commands in an explicit More menu and the selected
torrent's context menu. Retain their prerequisite explanations and destructive
confirmations. Global Pause all/Resume all must retain an explicit all-torrents
scope, including rows hidden by filters.

Use a real overflow behavior for narrow windows. A fixed-width magnet field or
long status message must not expand the virtual parent beyond the viewport or
consume the table's usable height. Agent access should remain accessible from a
compact entry without an always-visible two-line disabled banner.

Tests:

- Render wide, minimum-width and enlarged-text windows. Primary actions remain
  reachable, overflow exposes every secondary action, and no command disappears.
- Drive menu/input clicks through egui. Verify exact action intent and target IDs,
  including the disabled-copy explanation for an active torrent.
- Verify context-menu and command-bar actions use the same dispatch rules.
- Retain existing import, drag/drop, storage confirmation and scrolling tests.

## UI 2: configurable columns with a persistent header

Support all eight existing torrent columns: Name, Progress, Status, Download,
Upload, Peers, ETA and Ratio. Resize every column, hide optional columns, reorder
them by drag/drop and accessible move-left/right commands, and restore defaults.
Keep Name visible. Normalize duplicate/missing columns and out-of-range widths.

Clicking a header selects its sort and toggles direction; show a direction arrow.
Add the missing sort metrics to the shared presentation cache without reordering
the authoritative torrent vector. Use stable identity tie-breaking and defined
handling for unknown ETA/zero-length progress.

Pin the header during vertical scrolling while sharing the body's horizontal
offset and geometry. Keep the last visible column reachable at minimum width,
with correct scrollbar gutters. Width/order changes must preserve a valid offset,
existing Name fill behavior and the selected torrent's identity. Save the layout
through the UI-only settings path.

Tests:

- Model normalization and serde round trips, including legacy settings with none
  of the new fields, invalid order/width data and attempted Name hiding.
- Sort every column in both directions, equal metrics and missing ETA; retain
  selection and canonical vector order while live values change.
- Pointer-driven resize/reorder/hide/reset and header-sort tests.
- Scroll to the bottom/right in a narrow viewport. Verify the pinned header and
  matching row cells stay aligned and every visible column remains reachable.
- Retain the 5,000-row virtualization and existing Name resize regression tests.

## UI 3: identity-based multiple selection and scoped batches

Use canonical torrent IDs for the selected set, focus and range anchor. Plain
click replaces selection; Ctrl-click toggles; Shift selects a range in the current
filtered/sorted order; Ctrl+Shift unions that range. A missing or hidden anchor
falls back to the clicked row. Right-click on a selected row preserves its group;
right-click elsewhere selects that target before opening its menu.

Filtering may retain hidden selected IDs, but default batch actions operate only
on selected visible rows. Display visible/hidden counts and provide a clear-hidden
selection action. Freeze the target IDs when the user starts a batch. Reconcile
each current ID/revision immediately before dispatch and report partial rejection
per target; never substitute row indices or silently include hidden rows.

Provide Pause and Resume for the group. Removal opens a confirmation with target
count/names and explicit data scope. Default list removal keeps payloads. Data
recycling continues to use individual owner-issued, single-use path plans; a
multi-target confirmation cannot bypass their identity/revision checks.

Tests:

- Ctrl/Shift/Ctrl+Shift ranges, toggles and right-click preservation across sort,
  filters, removal/re-add and empty lists. Check focus/anchor pruning by identity.
- Hidden selection does not receive default batch commands. Zero visible targets
  produces no mutation. Selection gestures do not steal text-entry input.
- Simulated partial acceptance/Busy/Stale/error outcomes retain separate results
  and do not replay a rejected or ambiguous mutation.
- Confirm/cancel removal, target disappearance and stale trash plans with fake
  recycling and disposable synthetic paths only; no user payload deletion.

## UI 4: an actual selectable file hierarchy

Tree mode becomes nested folders with disclosure controls, whole-folder selection
and an indeterminate checkbox when only part of a folder is selected. Folder
selection explicitly applies to all descendant files, including search-hidden or
collapsed descendants; explain this scope in its tooltip. An indeterminate click
selects the whole folder; a checked click clears it. Existing visible-file priority
actions affect only displayed matching file rows and preserve all other bits.

Search exposes matching descendants and their ancestors without destroying the
user's pre-search expansion choices. Flat modes remain available. Retain original
file indices for reveal/priority actions. Keep the file count outside scrolling,
the frame/gutter, full-height content, left alignment and shared progress/size
columns. The single top-folder header remains fixed and becomes interactive.

Build/cache hierarchy and selection aggregates on the loaded file model. Progress
ticks must not rebuild the hierarchy or scan every descendant per visible row.
Defer/drop releases the heavy caches. Egui memory retains only small expansion
preferences, not cloned full file paths. Pending/cold details emit no speculative
selection, priority or navigation intents.

Tests:

- Nested folders, files beside folders, all/none/mixed descendants, and swaps
  between folders that keep the same overall selected count.
- Collapse/expand/search/clear-search preserves expansion and matching ancestors.
- Folder selection covers hidden descendants; visible priority changes preserve
  hidden/collapsed priorities. Reveal returns the original canonical file index.
- Cold/loaded/clone/defer cache lifecycle and stale-generation regressions.
- Existing fixed-count, scrollbar, width and progress-alignment tests, plus a
  large nested fixture verifying bounded rendered rows and cached reuse.

## UI 5: visible background-client status and shutdown

Show a persistent footer with native connection state and current aggregate
download/upload rates. Clearly distinguish connected, reconnecting, disconnected
and a user-requested stop. Stale transfer rows are retained as the last known
view, with an explanatory state and disabled unsafe mutations.

Expose separate Close window and Stop background client actions. Closing the
window retains transfers. Stop requires an explicit confirmation and targets the
displayed owner instance; an owner change invalidates that confirmation. Dispatch
and waiting run off the GUI thread. Do not automatically restart an owner just
stopped by the user. Admission is not proof of final durable shutdown; display
only the outcome actually established by the available receipt/lock evidence.

Tests:

- Headless rendering and clicks for connected/lost/reconnecting/stopping states,
  including zero totals and suppressed stale mutation controls.
- Cancelled confirmation emits no Stop. A replaced owner rejects an old confirmed
  Stop, and an accepted Stop is not automatically retried against a new owner.
- Copied candidate binaries on a disposable no-discovery profile: closing and
  reopening the frontend preserves one owner; explicit Stop releases ownership.
- Retain the permanent TCP seeding and Windows pipe-EOF lifecycle regressions.

## UI 6: bounded operation history and actionable errors

Replace the single overwrite-prone status line as the only source of feedback
with a bounded operation panel. Record requested operation, target, time, progress
and outcome independently. Preserve meaningful existing transfer/error feedback.
Show queued, accepted, running, applied/persisting, completed, failed and needs-
review states according to actual evidence; owner acceptance alone is nonterminal.

Use operation IDs and owner/revision correlation where available. Only matching
engine/effect and durability proof can establish that kind of completion. A
durable flag describing an earlier saved state cannot complete a new request.
Recheck start is progress, not successful verification. Missing correlation,
disconnect or replacement is shown honestly rather than guessed as success.

Bound retained rows and bytes, truncate oversized display text safely, and expose
overflow explicitly. Keep separate repeated operations and errors. Offer safe
Focus torrent, Copy error and Dismiss finished actions. Do not blindly retry
commands or recycle data from a notification. Avoid noisy success popups and
system notifications containing private names, paths or tracker credentials.

Tests:

- Repeated operations on one target, distinct IDs, bounded retention/overflow,
  Unicode truncation, safe action intents and independent failures.
- Out-of-order/duplicate/stale receipts, early durable snapshots, superseding
  revisions, disconnect/reconnect and unavailable evidence never produce green
  completion. Progress is bounded and cannot regress within one operation.
- GUI integration drives real command/receipt handlers and synthetic owner
  fixtures, checking history entry creation and final/ambiguous outcomes.
- A later successful navigation message cannot erase an earlier operation error.

## UI 7: coherent appearance and full-row state presentation

Provide System, Light and Dark theme choices; Compact and Comfortable density;
and a bounded text/zoom setting (75-200%, default 100%). System follows the OS.
Apply absolute zoom rather than multiplying the current DPI every frame. Theme,
density and zoom changes update both theme styles consistently and invalidate
dependent measurements without rebuilding them on every unchanged frame.

Use shared spacing and state tokens. Highlight a selected torrent across its
whole visible row, retain hover/focus feedback, and pair status colors with text
or icons. Keep numeric columns readable/right aligned where appropriate. Theme
and display settings save through UI-only persistence and take effect without an
engine restart or changing separately unsaved download preferences.

Tests:

- Default/legacy/invalid serde settings, zoom clamping and idempotent application.
- Light/dark/system switching, both densities, DPI/zoom changes and restoration
  avoid cumulative scaling or stale width caches.
- Render selected/hovered/error/checking/paused/done rows in both themes. Verify
  full-row coverage, readable state text, keyboard focus and non-color meaning.
- Render at 100/150/200% in wide/narrow windows and inspect isolated screenshots
  for overlap, clipping, gutters and unreachable controls.

## Test and delivery sequence

1. Save this plan and its release-index entry before source implementation.
2. Agree shared contracts, then implement independently owned model/widgets/tests.
3. Integrate actual app actions, native receipts, UI-only persistence and shutdown.
   Tests must exercise the integration, not only standalone helper functions.
4. Run focused unit/property and egui input/render regressions as each item lands.
5. Run formatting, strict Clippy, language/diff/actionlint and script contracts.
6. Run all-features, core-only and TUI application/vendored-tracker test matrices;
   ensure the GUI work does not introduce GUI dependencies into headless builds.
7. Produce an instrumented GUI coverage report where supported and add a Windows
   GUI coverage CI path. Preserve the existing measured Linux core/TUI baseline;
   never lower it to obtain a pass. Record actual new-module line/function counts,
   uncovered paths and tool/platform blockers without inventing a percentage.
8. Build a candidate from the same production entry points under separate output
   names if running executables are locked. Reuse caches; never overwrite or
   terminate the live client's executable. Test only disposable candidate owners.
9. Inspect wide/narrow, scroll and theme/zoom states, then record results for all
   seven items, actual tests, coverage and remaining platform checks.

Coverage is a delivery requirement: new state transitions, normalization, batch
scope, receipt ordering and failure paths need direct tests; each new visible
control needs a render/input integration test. A helper-only test that bypasses
the command handler is insufficient. Numeric coverage is reported from actual
instrumentation and reviewed alongside behavior assertions, not substituted by a
test count. Do not exclude uncovered new modules or weaken assertions for green.

Native Windows evidence does not establish Linux runtime/window behavior. Existing
Linux CI, installer/service tests and long swarm soaks remain distinct gates.
This request does not authorize a new unattended long run, client restart,
package publication, commit or push; perform those only when separately requested.

## Recorded implementation refinements

The following refinements preserve the acceptance criteria above and address
issues found by integration tests and independent review.

- Native frontend protocol 2 carries an authoritative file-metadata generation
  separately from command CAS revisions. File pages, whole-file selection and
  priority changes require both fences. A same-count selection ACK between pages
  invalidates the old view; progress-only updates preserve its file generation.
  The restricted external-agent protocol remains version 1. A new frontend must
  reject a legacy owner explicitly; it must not replace or stop that owner.
- A removal review freezes canonical identities, the displayed owner and each
  command revision. Confirmation rejects a replacement owner, changed revision
  or missing target instead of obtaining fresh authority for an old review.
- Owner snapshots retain their explicit durable flag alongside owner and command
  identity. An empty frontend persistence queue is never saved-state evidence.
- Preference submissions merge the current UI state into the preference snapshot;
  saving a view never submits unsaved global preference edits. Older responses
  cannot overwrite a newer live UI preview. Offline Apply/Save fails closed and
  retains the editor draft, including when no native client is attached.
- Quiet agent polling does not create operation-history entries. When the existing
  owner API supplies no correlated completion proof, the history says Outcome
  unknown instead of showing success or waiting indefinitely. Only the user may
  dismiss such a review entry explicitly; this removes its presentation record,
  without replaying, undoing or asserting completion of the underlying action.
- Live sorting during a pointer press cancels a click if its original row moved
  outside the actual release geometry. Stable widget identities alone are not
  sufficient because egui can retain a click from the previous frame's geometry.
- Every required new GUI source must appear in actual LLVM output with positive
  executed line and function counts. Local measurements are reported separately
  from the first CI numeric baseline; no guessed percentage gate is introduced.