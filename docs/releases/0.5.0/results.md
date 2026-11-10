# PeerLume 0.5.0 implementation evidence

The implementation is based on commit `2d4df223847e4ff07cf2db5f27ae38feefdc742b`.
This document records progress on the [release plan](release-plan.md).

## First increment

- The background owner applies Normal, Quiet and Automatic speed profiles and
  the active download slot limit to the current engine. Manual profiles stay
  selected until changed. Automatic evaluates configured weekdays and times on
  the local clock. GUI **More → Speed profiles** and TUI `:speed` show engine
  readback and keep it separate from saved settings and draft edits.
- GUI and TUI Status use the same reason and next-action model. The GUI table
  also shows it in the status tooltip. The model distinguishes confirmed queue
  admission, an active integrity check, pause, transfer errors, missing evidence
  and a disconnected view. Tracker failure is historical evidence when a
  matching announce is available. Redacted export includes the connection state.
- GUI **Preferences → Updates** can verify an extracted x64 portable desktop
  package. The verifier checks the allowed member set, bounded file sizes,
  executable format and architecture, the build manifest and all declared
  SHA-256 digests. The package displays its manifest identity. Local digest
  agreement establishes integrity relative to the supplied checksum list.
  Publisher trust and compatibility of the candidate with an installed daemon
  require separate verification.

Trusted native protocol version 3 introduced the speed commands and readback.

### Local Windows evidence

- The all-feature library suite passed 832 tests. Seven dedicated workloads
  remained ignored in this run.
- All three installed native lifecycle tests passed using disposable profiles.
- Eighteen Python packaging contract tests passed.
- Rustfmt and strict Clippy passed. Clippy checked all targets with all features
  and with TUI only. The core library also passed a build check with default
  features disabled.
- The native speed workflow verified applied versus saved values, profile
  changes with two frontends, detach and reattach, and engine and listener
  identity. A TUI test verified that switching profiles preserves a draft cap.
- Two concurrent loopback downloads changed limits in the same engine, kept the
  listener and verified both final payloads by hash. The focused re-run also
  measured wire pacing with the per-torrent cap and global limits disabled.
  The quota test exercised raising and clearing a limit while a peer was
  waiting for the prior quota.
- The GUI and model tests exercised confirmed queue waits, stale evidence and
  privacy-safe diagnostics. Package fixtures exercised Windows and Linux
  archive layouts, corrupted contents and invalid package members.

## Second increment

- Startup recovery displays the profile location and offers retry and
  configuration validation in the GUI and TUI. The daemon writes one typed
  startup failure record under its profile lock. A successful startup clears
  the record. Configuration validation and stop requests leave the record
  alone. The recovery message uses fixed text for the observed stage.
- The Operations window and the TUI `:operations` command show the same paged
  history from the daemon. The owner distinguishes requests that reached it,
  engine effects, durable success and outcomes that need review. It retains up
  to 256 records and preserves uncertainty after owner replacement. It checks
  command revision and available engine evidence before reporting success.
  Copy progress shows measured bytes. The total is available for checks.
  If all retained slots contain active work, new tracked requests receive a busy
  reply. If the history store is unavailable, the frontend uses local receipts
  and reports that shared history is unavailable.
- Native protocol version 4 carries operation pages and request correlations.
  GUI, TUI and daemon need matching versions. The restricted public control
  channel keeps its existing protocol and permissions.

### Local Windows evidence

- The cumulative all-feature library suite passed 851 tests. Seven dedicated
  workloads remained ignored. All five native lifecycle tests and 18 Python
  packaging contract tests passed. Rustfmt and strict Clippy passed for
  all-feature and TUI-only targets.
- Copied daemon and frontend executables exercised invalid configuration,
  corrupted SQLite state, lock contention, a read-only configuration check and
  successful startup after repair. The frontend received the fresh typed
  failure and the healthy startup cleared it.
- The native resume test blocked the SQLite acknowledgement with an injected
  trigger. The operation showed the confirmed engine effect until the durable
  write succeeded. Additional tests covered delayed feedback, a manual recheck
  with concurrent verification evidence, unavailable history, owner replacement,
  bounded retention and the real TUI view save.
- The occupied-listener integration scenario targets Linux. The Windows run
  covers the platform-independent startup classification and record handling.

## Remaining acceptance

Magnet review, watched imports, category rules, completion moves, tray
notifications and upgrade activation remain in the release plan. Upgrade activation includes a trusted release channel,
profile backup, compatible handoff and rollback.

Linux runtime, installed desktop interaction, GitHub workflows, coverage
measurement and a bounded soak remain open for this increment.
