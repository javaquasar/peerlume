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
- `app`: CLI now; TUI or HTTP API later.

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

The sample settings are in `config/peerlume.example.toml`.

## Private torrents

Private torrent requirements and tracker accounting rules are documented in
`docs/private-torrents.md`. Production tracker reporting supports `accurate` and
`disabled` modes. Disabling reports also disables tracker communication and takes
effect after restarting the application.

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
- editable tracker URLs for reviewing or replacing account passkeys received from
  another user, plus a generated and editable 20-byte peer ID;
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
client remains limited to accurate reporting or complete tracker disablement.

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
