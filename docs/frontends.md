# Frontend architecture

PeerLume has one protocol, persistence, and transfer core with optional user
interfaces. UI code must not open SQLite directly or talk to `librqbit` directly.

## Shared layers

- `client.rs` owns the `ClientBackend`, state restoration/persistence, conversion
  between `TorrentRecord` and `TorrentItem`, transfer commands, and the common
  `SessionEvent` reducer.
- `ui_schema.rs` is the canonical list of pages, torrent detail tabs, preference
  sections, and feature components. Both frontends use its labels and ordering.
- `torrent.rs`, `settings.rs`, `tracker.rs`, and the protocol/storage modules are
  UI-independent.
- `app.rs` and `preferences.rs` contain eframe/egui rendering only and are behind
  the `gui` Cargo feature.
- `tui.rs` contains ratatui rendering, terminal input, and prompt state only and is
  behind the `tui` Cargo feature.

Any new screen or action must first be represented in `ui_schema.rs` or in the
shared client model. A frontend-specific implementation is then added to both
frontends. This keeps navigation names, ordering, state transitions, persistence,
and transfer behavior identical.

## Running

Desktop GUI:

```powershell
cargo run --bin peerlume
```

Terminal UI without compiling the GUI stack:

```powershell
cargo run --no-default-features --features tui --bin peerlume-tui
```

TUI keys:

- `1` / `2`: Torrents / Preferences;
- arrows: select torrent, file, or preference section;
- `Tab` / `Shift+Tab`: torrent detail tabs;
- `a`, `m`: add a `.torrent` path or magnet URI;
- after a `.torrent` path, select a download mode before confirming addition;
- `p`, `r`, `P`, `R`: pause/resume one or all torrents;
- `d`: remove the selected torrent while keeping downloaded data;
- `v`: recheck the selected torrent's files, keeping data and session transfer totals;
- `V`: toggle the selected torrent's persistent Verify after download setting;
- `F3`: toggle Verify after download while reviewing a pending torrent's mode;
- `e` / `E` in Log: export selected-torrent / entire-session JSONL to a new file;
- up/down in Log: scroll real announces and returned peer addresses;
- `Space`: toggle the selected file or the primary option on a preference page;
- `o`: edit a per-torrent download directory in Torrent settings;
- `u`, `A`: edit Tracker URL or run the saved audit scenario in Trackers;
- `F2`: show/hide URL-derived tracker account fields in Info, Trackers, or the
  add-torrent mode prompt (these fields are not the client's Peer ID);
- `b`, `x` in Trackers: reload/apply an external behavior profile by ID, or cancel
  the running tracker scenario;
- `s`, `D`: validate/save preferences or load defaults;
- `q` / `Ctrl+C`: exit.

## Parity contract

Both frontends expose the shared torrent table, add torrent/magnet operations,
pause/resume/remove actions, file selection, Info, per-torrent Settings, peer
counts, tracker audit, piece progress, real announce journal/JSONL export, and all nine preference
sections. The terminal layout uses prompts and keyboard actions where egui uses
dialogs and widgets.

`announce_journal.rs` owns the shared bounded live tail and flushed disk log.
The transport records actual and sent metrics at the request boundary, before
forwarding response peers to the download engine. Neither UI synthesizes announce
records from periodic progress notifications. The standalone audit remains separate.
File selection changes are forwarded to the common session and persisted, so the
same partial-download selection survives recheck and restart. The freeze threshold
is entered before addition (a numeric field in egui, an extra prompt in ratatui).

The complete tracker scenario editor remains stored in the common SQLite schema.
Both frontends load the same external behavior catalog, resolve it into a saved
per-torrent snapshot, and execute all expanded steps, including delays and
started-only observation. The egui frontend additionally provides visual step
insertion/reordering. Configuration is documented in `tracker-audit-scenarios.md`.

## Tracker account identity

The egui add dialog shows **Tracker account / user identity** directly below the
single editable Tracker URL, in every download mode. The same block appears in
the selected torrent's Info and Trackers tabs. Ratatui shows the same derived
fields before download-mode confirmation and in Info/Trackers. Values are hidden
until **Show** (egui) or `F2` (ratatui); changing the URL hides the newly selected key.

`identity.rs` identifies URL-declared `uk`, passkey/token/authentication keys,
explicit `uid`/`user_id` fields, URL userinfo, and possible keys in the URL path.
These are candidates from local metadata, not an authenticated account lookup.
For example, `uk` may be the account's tracker authentication key; it is not the
client Peer ID and does not reveal the numeric account ID behind that key.
If no supported field exists, the UI explicitly says no account ID/key was
recognized instead of inferring one from the info hash or Peer ID.

The block is read-only and always derives its values from the current Tracker
URL. Changing that URL updates the displayed fields immediately; the normal
URL persistence applies, without a second independently editable copy of the key.
Account fields are not added to JSONL or ordinary diagnostic logs.

## Tests

Run all frontends and shared-code tests:

```powershell
cargo test --all-features --all-targets
cargo clippy --all-features --all-targets -- -D warnings
cargo check --no-default-features --all-targets
```

Ratatui screens are rendered into `TestBackend` for every page, detail tab, and
preference section. Shared tests cover schema completeness, navigation cycles,
event reduction, record conversion, command construction, input prompts, bounded
selection, file toggles, and tracker-audit defaults.

The previously measured all-feature baseline was 55.33% line coverage (before the
external behavior catalog). `scripts/coverage.ps1`
enforces a 55% project-wide floor so coverage cannot silently regress while the
callback-heavy GUI is progressively moved into testable shared reducers. Install
`cargo-llvm-cov` and run:

```powershell
.\scripts\coverage.ps1
```
