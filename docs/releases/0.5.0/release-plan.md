# PeerLume 0.5.0 release plan

Plan revision 1.0 targets PeerLume 0.5.0. Implementation has started.

Baseline commit `2d4df223847e4ff07cf2db5f27ae38feefdc742b` passed the
Windows and Linux package matrix, Tests, Reliability and Coverage.
This version identifies the plan. Packaging version and release publication
are separate release decisions.

## Product outcome

Users can control a running transfer session, understand why a torrent is
waiting, review what the owner actually completed, and safely automate routine
import, organization and upgrade work. The background owner remains the source
of truth. GUI and TUI present the same acknowledged state and use the existing
trusted local channel for mutations.

## Delivery order

1. Live limits and truthful startup/transfer diagnostics.
2. Owner operation receipts and history, followed by magnet metadata review.
3. Category rules, watched imports and verified completion moves.
4. Desktop tray and notifications, followed by verified update and recovery.

Read-only package preview may proceed alongside the first increment. Package
activation follows the update and recovery gates. Each increment includes
tests for the real entry points it changes. Record actual
results and outstanding gaps in a release results document after validation.

## 1. Live speed profiles and scheduling

- Apply validated global download/upload caps to the existing session limiter.
  Preserve per-torrent caps and the identity of the session and listener. Keep
  the desired, applied and saved values distinguishable while a request is in
  flight. The applied state requires a reply from the engine.
- Offer a Normal/Quiet switch with the effective caps visible in both frontends.
  Persist the selected policy until the user chooses another. Automatic
  follows the schedule while it is selected.
  Use the existing alternate limits, selected days and clock times. Evaluate
  the policy in the daemon, including when no frontend is attached.
- Treat schedules as local wall-clock half-open intervals. Overnight intervals
  belong to the day they start. Re-evaluate on restart, resume from sleep and
  clock changes. Reject invalid values before committing either limit. Keep
  listener settings and other restart-required options explicitly scoped.
- Cover normal/quiet/zero caps, range and unit validation, week rollover, exact
  boundaries, overnight windows and DST re-evaluation with an injected clock.
  Exercise real global limiter changes during two concurrent loopback transfers,
  enforce the per-torrent override, compare persisted vs applied state, and
  verify session/listener continuity through both frontends disconnecting.

## 2. Actionable waiting and startup diagnostics

- Add a compact reason and a next action to the existing torrent status surface.
  Prefer confirmed evidence such as a paused state, queue admission, a missing
  path, a tracker failure or a pending check. Distinguish unknown/incomplete
  evidence from a confirmed root cause. Derive the same wording in GUI and TUI.
- Preserve a bounded, sanitized failure from daemon startup and show it in a
  recovery view with retry, configuration validation and a profile location.
  Keep the profile lock and native compatibility checks authoritative. Route
  retry to the current owner and retain the last known view on disconnect.
- Cover priority of multiple observed reasons, stale/unknown telemetry,
  paused or seeding rows at zero throughput, recovery after a slot becomes
  available, invalid configuration, unavailable credentials, occupied listener
  and inaccessible payload root. Assert that diagnostics omit tokens, private
  tracker credentials and unrelated profile data. Verify visible GUI/TUI actions.

## 3. Owner operation progress and outcomes

- Correlate commands with a bounded owner operation ID, target identity and
  command revision. Acknowledge queueing and acceptance separately from running,
  terminal engine feedback and confirmed persistence. Include bytes/total for
  supported copy and move operations. For uncertain outcomes retain an explicit
  review state. Correlate concurrent or superseding actions independently.
- Provide paged history and focus/error actions in GUI and TUI. Reconnecting
  frontends read the same owner records. Cap entries and text bytes, protect
  pending records, and expire or archive completed entries according to a clear
  retention rule. Retain uncertainty across owner replacement.
- Cover accepted then rejected, partial copy, persistence failure, stale
  revision, reordered receipts, disconnect and reconnect, owner replacement,
  two concurrent jobs, history bounds and successful cleanup only after a
  verified copy. Assert that an operation never displays premature success.

## 4. Magnet metadata review before payload start

- Add an explicit metadata review state using the existing owner, discovery
  controls and metainfo validation. Bound concurrency, bytes and elapsed time.
  Cancel review on request and suppress late results. Keep the payload engine
  inactive during metadata review. Apply private discovery rules as soon as
  the resolved metadata identifies the torrent as private.
- Reuse the file tree and add dialog for name, sizes, selection, destination
  and start policy. Revalidate the info hash, limits and paths before the final
  add. Handle duplicate, stale and unsupported metadata as visible outcomes.
- Use a synthetic loopback metadata exchange to verify that there are zero piece
  requests and payload files before confirmation. Cover cancellation, timeout,
  duplicates, malformed or mismatched metadata, private discovery, large trees
  and single-file and multifile review. After confirmation, verify the selected
  payload against the piece hashes.

## 5. Category rules, watched imports and completion placement

- Store rules as owner policy with explicit precedence and an impact preview.
  Pin destinations and limits when a torrent is admitted. A category edit or
  rule change on existing rows requires an explicit target and preview before
  it schedules data relocation or changes active limits.
- Consume only validated, stable regular `.torrent` files in configured watch
  roots. Bound import work, ignore partial files and links, deduplicate by
  canonical identity and record outcomes across restart. Keep the original
  available until registration is durably confirmed.
- Schedule completion moves from verified engine completion using the existing
  storage admission, path confinement and verified relocation machinery. Make
  collisions and shared roots explicit, preserve pause intent, and make recovery
  idempotent across a crash between copy, verification and path commit.
- Cover atomic writes, partial and repeated files, links, duplicate hashes,
  disabled or invalid rule, precedence and preview, insufficient free space,
  shared roots, cross-volume copy, injected interruptions at commit boundaries,
  restart and failed verification. Exercise all fixtures in disposable roots.

## 6. Tray controls and completion/error notifications

- Add opt-in tray support on supported desktops. Show aggregate rates and
  active count from the owner, open the existing window and offer explicit
  pause/resume actions. Preserve the difference between hiding the window,
  closing the frontend and stopping the owner. On unsupported desktops keep
  the same window controls available.
- Coalesce notifications and emit completion only after verified completion.
  Use privacy-preserving summaries by default and keep the event target safe
  to open after filtering or owner changes. Report unsupported delivery in the
  UI. No tray component is required by the headless packages.
- Cover tray intent routing, duplicate suppression, current-owner binding,
  no notification from unverified progress, privacy text and opt-out. On Windows
  and Linux verify actual desktop hide/open, notification and shutdown behavior.
  Check that the TUI dependency graph still has no GUI or tray libraries.

## 7. Verified update of the complete application package

- Use a configured trusted release channel and a versioned manifest identifying
  platform, complete binary set, source SHA, checksums and native compatibility.
  First show availability and allow inspection. Stage a package in a new local
  directory and validate filenames, architecture, member count, bounded sizes
  and hashes before preparing any replacement. Require a trusted manifest
  signature before automatic installation can be enabled.
- Show the proposed upgrade and requested interruption. Take a consistent
  profile backup, request graceful owner shutdown, wait for the existing lock
  to release, then replace the complete compatible pair and companion tools.
  Health-check attachment and profile schema. Keep a recoverable previous
  package and choose a schema-compatible rollback strategy. Private credentials
  remain in their platform secret provider.
- Cover wrong platform, version or hash, unsafe archive paths, oversized or
  missing members, signature and channel failure, network interruption, active
  profile lock, shutdown and health-check failure, full package replacement,
  upgrade/rollback with a disposable populated profile, and no mixed native
  protocol pair. Test Windows and Linux separately using actual package formats.

## Cross-cutting acceptance and release gates

- Keep public agent permissions and protocol separate from trusted native
  capabilities. Version native compatibility when a required wire field or
  command contract changes. Test old profiles and explicit mismatch handling.
- Apply finite queue, file, byte and time limits. Keep network and disk work off
  the render thread. Use real local loopback and isolated storage fixtures for
  engine and persistence assertions. Do not rely on a private tracker or live
  user profile in tests.
- Run Rust formatting, strict Clippy, core-only, TUI and all-feature suites,
  installed package lifecycle tests and the Python packaging contracts. Run
  Tests, Reliability, Coverage and all four Build packages jobs for the exact
  candidate commit. Preserve current coverage gates and measure new modules.
- Exercise Windows and Linux native UI surfaces, including shutdown, reconnect,
  upgrade and supported tray behavior. Measure latency and disk/network resource
  regressions during a bounded soak. The recorded workload and measured limits
  must accompany any release claim.
- Keep release evidence separate from plans. Report failed, skipped and unverified
  acceptance cases and use those results to decide release readiness.
