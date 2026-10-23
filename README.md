# PeerLume

Experimental BitTorrent client in Rust with configurable peer discovery, scheduling, rate limits, and protocol behavior.

## Project direction

Keep protocol mechanisms separate from policy:

- `metainfo`: parse and validate `.torrent` metadata and magnet links.
- `tracker`: HTTP/HTTPS and UDP announces.
- `discovery`: tracker, DHT, PEX, and LSD providers.
- `peer_wire`: BitTorrent handshake and peer messages.
- `scheduler`: piece selection, choking, retries, and connection limits.
- `storage`: piece cache, verification, resume data, and file layout.
- `policy`: replaceable behavior selected from configuration.
- `client`: UI-independent persistence and transfer-session facade.
- `app` and `preferences`: optional eframe/egui frontend, compiled by the `gui` feature.

Core behavior should be expressed behind traits such as `PeerSelectionPolicy`,
`PieceSelectionPolicy`, and `ChokePolicy`. Configuration selects implementations;
protocol code remains independent from those choices.

## Recommended crates

### Start with these

- `tokio`: asynchronous runtime, TCP/UDP, timers, tasks, and channels.
- `bytes`: efficient network buffers and wire-protocol parsing.
- `serde` + `toml`: typed configuration.
- `serde_bencode`: `.torrent`, tracker, and DHT bencode parsing.
- `reqwest` with `rustls`: HTTP/HTTPS tracker requests without an OpenSSL dependency.
- `sha1`: BitTorrent v1 info hashes and piece verification.
- `url`: tracker and magnet URL parsing.
- `tracing` + `tracing-subscriber`: structured diagnostics.
- `thiserror`: errors for library modules.
- `anyhow`: top-level CLI error reporting.
- `clap`: command-line interface.

### Add when needed

- `mainline`: mature BEP 5 DHT implementation; wrap it behind a discovery trait.
- `socket2`: socket options not exposed by the standard high-level APIs.
- `bitvec`: compact piece availability maps.
- `futures`: stream combinators where Tokio alone is insufficient.
- `governor`: rate limiting for peer and tracker traffic.
- `ratatui` + `crossterm`: terminal interface.
- `axum`: optional local control API.
- `librqbit`: the production transfer backend used by the desktop prototype for
  tracker discovery, peer connections, verified piece transfer, and file storage.
  PeerLume keeps its UI, persistence, selection, and policy models around it.

## First milestone

1. Read a `.torrent` file and print validated metadata.
2. Announce to one HTTP tracker.
3. Connect to one peer and complete the handshake.
4. Download and verify one piece.
5. Move peer and piece choices behind configurable policy traits.

## Run

```powershell
cargo run
```

The default `gui` feature builds the desktop frontend. The reusable core can be
compiled without any windowing or file-dialog dependencies, which is the contract
used by the ratatui binary:

```powershell
cargo check --no-default-features --lib
```

A terminal frontend should own only terminal navigation/rendering state and use
`client::ClientBackend` for restored torrent records, transfer commands, session
events, and persistence. Protocol, settings, torrent, tracker, and storage modules
are available without the `gui` feature.

Run the terminal interface without compiling the GUI stack:

```powershell
cargo run --no-default-features --features tui --bin peerlume-tui
```

The shared component contract, parity rules, and TUI key map are documented in
`docs/frontends.md`.

The sample settings are in `config/peerlume.example.toml`.

## Private torrents

Private torrent requirements and tracker accounting rules are documented in
`docs/private-torrents.md`. Global tracker communication supports `accurate` and
`disabled` settings. Disabling reports also disables tracker communication and takes
effect after restarting the application.

When adding a `.torrent`, choose Normal download (the default), Download without
reporting progress, reported upload = downloaded × 1.5, hide downloaded only, or
freeze metrics after a configurable received-byte threshold. All five
download data from peers; the chosen mode is saved per torrent and restored after
restart. The global `disabled` setting still disables all tracker communication.

The selected torrent's **Log** tab shows real HTTP/HTTPS and UDP download announces:
actual vs sent counters, checked disk bytes, events, tracker replies and returned
peer addresses. JSONL is written continuously to
`%LOCALAPPDATA%/PeerLume/announce-logs/`; exports read the full disk history rather
than the bounded live tail. In egui use the export buttons; in ratatui use `e`
(selected torrent) or `E` (session), and `v` to recheck files without deleting them.
See [tracker audit scenarios](docs/tracker-audit-scenarios.md#real-announce-journal-and-jsonl)
for accounting semantics, long-running tests, and journal limitations.

The isolated long-running started-only audit mode is documented separately in
`docs/started-only-observation.md`.

Tracker audit modes and combinations can be defined in
`%LOCALAPPDATA%/PeerLume/tracker-behaviors.toml`, shared by egui and ratatui. The
default file is initialized from `config/tracker-behaviors.example.toml` when the
catalog is first loaded. Set `PEERLUME_TRACKER_BEHAVIORS` to use another file.
See `docs/tracker-audit-scenarios.md` for the format, reload controls, and snapshot
semantics.

## Desktop UI

Run `cargo run` to open the eframe/egui prototype. It supports per-file checkboxes,
Select all / Select none, selected size calculation, and produces file indices for
the future torrent session. Integration rules are in `docs/selective-download.md`.

## Integrated desktop prototype

The current desktop prototype includes:

- a torrent table with progress, state, transfer rates, peers, seeds, ETA, and ratio;
- torrent selection with Files, Info, and per-torrent Settings detail tabs;
- a grouped file tree with per-file selection and progress;
- an add-torrent review window that parses v1, v2, and hybrid metainfo, shows the
  structure before starting, and lets the user select files;
- drag-and-drop import queue for local `.torrent` files, with a visible drop target,
  file-type validation, and sequential review before anything is added;
- exact info hashes calculated from the original bencoded `info` bytes;
- one authoritative editable Tracker URL shared by audit and normal transfer, plus
  a generated and editable 20-byte audit Peer ID;
- an isolated tracker-audit panel directly below torrent metadata in the add dialog,
  with editable `uploaded`, `downloaded`, `left`, event and port values plus the
  returned compact IPv4 peer list;
- a per-torrent audit scenario editor in the Trackers tab, with ordered steps,
  per-step delays and accounting values, individual-step or full-scenario execution,
  cancellation, and a `sent -> received` exchange log;
- an optional started-only observation mode that sends one zero-counter `started`
  announce with the full torrent size in `left`, then waits for up to seven days
  without sending periodic, `completed`, or `stopped` audit announces; selecting it
  in the add dialog forces the normal transfer to start paused and launches the
  observation immediately after the torrent is added;
- persistence of each torrent's audit steps, staging allowlist host, announced port,
  and Tracker URL; on Windows the exact credential-bearing URL is protected with
  current-user DPAPI while SQLite and logs expose only its redacted form;
- Downloads, Queue, Speed, Connection, BitTorrent, Seeding, Privacy, Storage,
  and Automation preference tabs;
- global and per-torrent limits, peer discovery switches, piece strategy,
  encryption policy, queue limits, ratio/time seeding limits, proxy and bind settings;
- accurate tracker accounting or complete tracker communication disablement;
- Apply, Save, Cancel, and Reset actions with validation and an explicit runtime
  application mode for each settings group;
- persistent settings in `%LOCALAPPDATA%/PeerLume/settings.toml`;
- automatic migration of the earlier flat settings file to the version 2 schema;
- a background Transmission-format gzip IP blocklist updater;
- a streaming blocklist decoder and compact binary cache in
  `%LOCALAPPDATA%/PeerLume/blocklist-v2.bin`, with IPv6 ranges and CRC validation;
- an IP probe that demonstrates the same `is_blocked` lookup intended for peer admission;
- tracker-only discovery enforcement for private torrents: DHT, PEX, and LSD are
  disabled by the effective settings and by the transfer backend;
- a real `librqbit` transfer session that restores saved torrents, writes into the
  selected directory, honors selected file indices, and reports live progress,
  rates, peer counts, pause/resume state, completion, and errors to the UI;
- context-menu actions for moving downloaded files, the source `.torrent`, or both
  to the operating system Recycle Bin/Trash after the transfer engine closes them.

Run the desktop UI:

```powershell
cargo run
```

Update and validate the configured blocklist without opening the UI:

```powershell
cargo run -- --update-blocklist
```

Run the tests and strict lint check:

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
```

## GitHub Actions

The [Tests workflow](https://github.com/javaquasar/peerlume/actions/workflows/ci.yml)
runs on pushes to `main` or `master`, including merge commits, and on pull requests
targeting either branch. The current default branch is `main`.

To start a run manually, open **Actions > Tests > Run workflow**, select a branch,
and confirm **Run workflow**. With GitHub CLI, use:

```powershell
gh workflow run ci.yml --repo javaquasar/peerlume --ref main
gh run list --repo javaquasar/peerlume --workflow ci.yml
```

CI checks formatting, strict Clippy lints, and repository language. The test matrix
covers both GUI and TUI features on Windows and separate core-only and TUI builds
on Linux, including the vendored tracker library's tests and documentation tests.
Linux GUI builds are not included because the GUI dependency does not currently
enable Linux window-system features. A separate Ubuntu job runs the four ignored
Docker tracker/peer integration tests against local synthetic fixtures; it does
not use production trackers or private torrent credentials.

Dependencies are resolved from `Cargo.lock`. Actions are pinned to commit hashes,
workflow permissions are read-only, and superseded runs for the same branch or
pull request are canceled. A failed matrix entry does not cancel other entries.

## Integration tests

Run the Docker integration suite with Docker Desktop or another compatible Docker
daemon running. The suite uses `testcontainers` and pulls `python:3.13-alpine`:

```powershell
cargo test --test container_network -- --ignored --test-threads=1
```

These container tests validate exact HTTP tracker accounting, the BitTorrent
handshake, verified piece persistence, and rejection of corrupt peer data. The
separate `core_workflows` integration suite covers synthetic metainfo preview,
selective piece planning, and file-backed SQLite resume state without requiring
Docker.

The `session::multi_peer_tests` suite runs the real transfer backend against two
or three local peers, each advertising a disjoint set of multi-block pieces.
Every peer withholds its first block until all peers have outstanding requests:
serial peer downloads cannot pass this synchronization gate. Tests verify each
peer's payload contribution, completion, exact output bytes, and every piece hash,
in normal and no-progress reporting modes. These tests require no Docker or public
tracker and run automatically in every CI test-matrix entry.

The same suite also adds two different `.torrent` files to one `SessionRuntime`,
with distinct content hashes, payloads, local trackers, peer sets, and output
directories. One uses normal reporting and the other hides progress. A shared
first-block gate and a second gate halfway through each peer's payload require
all four connections across both torrents to make progress before any can finish.
Downloading the torrents one after another cannot pass. Both torrents must report
completion independently, and both output files and all piece hashes are checked.
The first-block and midpoint gates also protect the single-torrent tests.
Negative controls verify that an incomplete peer group cannot pass the first
gate and one torrent alone cannot open the midpoint gate. Run the suite with:

```powershell
cargo test --locked --all-features --lib session::multi_peer_tests
```

This proves concurrent peer transfers, not one operating-system thread per peer
or a particular throughput improvement. The backend uses asynchronous tasks on a
multi-threaded Tokio runtime.

Desktop rows now show live values from the `librqbit` multi-peer transfer session.
The internal protocol modules also contain tested HTTP and UDP announces, tracker
tiers and retry state, peer handshake/messages and extension messages,
rarest-first/sequential piece scheduling with endgame requests, selected-range
piece mapping, bounded block buffers and rate limiting, SHA-1/SHA-256 verification,
positioned storage, verified magnet-metadata assembly, and an end-to-end local
single-piece transfer. SQLite in WAL mode persists torrent, file, tracker, queue,
statistics, and transactional verified-piece checkpoints.

The detailed milestones and release gates are in `docs/release-plan.md`.
Defensive tracker-accounting tests and anomaly scenarios are documented in
`docs/tracker-audit-scenarios.md`.

## Tracker accounting audit utility

`tracker-audit` sends one controlled announce with caller-supplied accounting
counters and prints the returned compact IPv4 peer list as JSON. It accepts loopback
trackers by default; a non-loopback staging hostname must be repeated exactly in
`--allow-host`. Use a dedicated test passkey and torrent.

The same one-shot operation is available in the desktop add-torrent dialog under
**Tracker audit**, immediately below the torrent summary. Its counters affect only
that audit request and are not reused by the normal transfer session.

```powershell
$env:PEERLUME_AUDIT_TRACKER_URL = "http://127.0.0.1:8080/announce"
cargo run --bin tracker-audit -- `
  --info-hash 0123456789abcdef0123456789abcdef01234567 `
  --uploaded 0 --downloaded 0 --left 1048576 --event started `
  --run-id downloaded-zero-01
```

For an authorized staging tracker, add its exact hostname:

```powershell
cargo run --bin tracker-audit -- `
  --allow-host tracker.staging.example `
  --info-hash 0123456789abcdef0123456789abcdef01234567 `
  --uploaded 0 --downloaded 0 --left 1048576 --event started
```

Run the command repeatedly with the same peer ID and deliberately chosen counter
transitions to exercise the scenarios in the audit document. The production desktop
client uses accurate reporting by default; the separate per-torrent Download mode
selector enables the four explicit download-time metric test modes.

## Memory behavior

- Bencoded byte strings borrow slices from the input buffer while parsing. Large
  `pieces` values are not copied, and unused top-level fields such as v2 piece
  layers are skipped without constructing an object tree.
- Imported file paths move directly into the selection model. The torrent name is
  not repeated in every multifile path.
- File tree rows and selection totals are cached. The UI renders only visible rows,
  and download progress is applied through incremental file events.
- The gzip blocklist is decoded line by line, merged in place, shrunk to its final
  capacity, and persisted as two `u32` values per IPv4 range.

The test suite includes a 100,000-file model, borrowed bencode slices, streaming
gzip parsing, binary cache round trips and corruption handling, and incremental
selection and progress accounting.

## Repository language and publication checks

All maintained source code, UI text, comments, documentation, configuration, and
file names must be in English. Unicode punctuation and UI symbols are allowed.
Before preparing a GitHub commit, run the language checker from PowerShell:

```powershell
powershell -NoProfile -File scripts/check-english.ps1 -SelfTest
powershell -NoProfile -File scripts/check-english.ps1
```

The checker scans tracked and untracked publication files using the repository's
Git ignore rules, rejects Cyrillic in paths or text, and reports invalid text
encodings. It checks UTF-8 and BOM-marked Unicode text; binary resources are not
interpreted as prose. Generated build output and local downloads remain ignored.
Automated detection is a guard against Cyrillic, not a semantic language detector:
review new prose to ensure it is English. This check does not replace a separate
credentials, licensing, and release review before publishing a repository.
