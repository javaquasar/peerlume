# Terminal frontend

Build without a desktop stack:

```sh
cargo build --release --locked --no-default-features --features tui --bin peerlume-tui --bin peerlume-control --bin peerlume-mcp --bin peerlume-daemon
./target/release/peerlume-tui --profile /absolute/path/to/profile
```

Windows uses the same options with `.exe` and an absolute Windows profile path.
Do not run GUI and TUI engines against the same profile. The existing instance
lock rejects a second owner. CLI/MCP adapters attach to the existing owner only
after explicit authorization; F8 access controls and credential protections are unchanged.

## Navigation and commands

Search also matches saved categories and labels. Additional view/storage commands
are described in [runtime controls](runtime-controls.md).

- Tab, Shift+Tab or F6 switches focus between torrents and details. Up/down acts
  on the focused pane; left/right switches detail tabs. Narrow terminals stack
  the panes. Alt+arrows changes their split (20..80 percent).
- `/` searches torrent names, `f` cycles status, Space marks rows, `*` marks all
  visible rows and Esc clears marks. Filtering clears hidden marks. Commands use
  stable torrent IDs, not display row numbers.
- `p`/`r` pauses/resumes marked rows or the current row. `P`/`R` applies to all
  torrents, including hidden rows. These keys queue requests, not completion.
- `v` requests a hash check. `c` confirms a full check and bounded repair; missing
  or corrupt pieces download using normal peers. A paused torrent may resume.
- `d` confirms removal **from the client only**. Downloaded payload and source
  `.torrent` files are kept. Confirmation pins IDs and command revisions;
  changed or removed targets are rejected, not silently retargeted.
- F1 opens help. `:` opens a fixed command palette (no shell execution): `pause`,
  `resume`, `check`, `repair`, `remove`, `search TEXT`, `split PERCENT`, `clear`,
  `preferences`, `torrents`, `help`, and `set FIELD VALUE`.
- Ctrl+C exits even in a prompt. `q` exits outside prompts.

## Preferences

Press `2`: left/right selects one of nine shared settings sections; up/down
selects **any** field, Enter edits, Space toggles a boolean. Changes form a draft.
`s` validates the complete draft and saves it; Esc discards it. Invalid values
never replace the saved settings. Proxy/script values are masked in the list
and entry prompt. There is no need to serialize a password into a command palette.

Saving does not restart or reconfigure active transfers. Each field identifies
its application scope. The shared GUI model has fields not implemented by the
engine; those are explicitly labeled **stored only**, not promised to work after
a restart. Queue admission and inherited transfer defaults take effect at next
startup/new torrent registration. Existing per-torrent overrides win.

## Running without a terminal

`peerlume-daemon` owns the same controller and engine without ratatui, egui or a
display server. Configure an **absolute** `download.download_directory` and
absolute per-torrent overrides first: relative paths depend on a service's working
directory. `--check-config` validates without creating a profile, engine or IPC
endpoint. `--help` and `--version` are also offline. Start with:

```sh
peerlume-daemon --profile /absolute/profile --enable-control
```

Do not keep GUI/TUI running against that profile; the one-owner lock applies.
Control is opt-in and only accepts existing tokens granted in the trusted GUI/TUI.
Grant/copy a token there first, then close that frontend before starting the daemon.
The daemon does not grant permissions, print tokens, auto-approve destructive
operations or start a second engine for adapters. Without `--enable-control`, no
agent access is enabled. Ctrl+C and Unix SIGTERM trigger final counter checkpoint
and clean session shutdown. Linux private credentials still need an unlocked
Secret Service in the service user's session; unavailable credentials fail startup.

The Debian package includes `peerlume.service` under `/usr/lib/systemd/user`.
It is never enabled automatically. After configuring and authorizing the profile,
`systemctl --user start peerlume` is an explicit opt-in start; check the user
journal for failures. Stop it before opening GUI/TUI. No system-wide root daemon
or Windows service registration is provided.

## Torrent controls

F7 edits seven values: download KiB/s, upload KiB/s, max peers, sequential
(`true`/`false`), priority (`low`/`normal`/`high`), seed ratio, seed minutes.
Zero disables a rate or seeding stop condition. Peer count is 1..4096.
Changing the existing payload directory is deliberately not offered as a saved
model-only override: relocation is not implemented. Download preferences affect
new torrents, and never move existing files.
Individual examples:

```text
:set download_kib 2048
:set upload_kib 512
:set sequential true
:set priority high
:set ratio_milli 2000
:set seed_minutes 60
```

Priority controls admission to `queue.max_active_downloads` slots, not upload
bandwidth. Equal priorities use engine registration order. With queueing disabled
all downloads are admitted. Already in-flight requests can finish after a queue
or sequential change; sequential order is best-effort across peers, not a promise
of contiguous playable data. Normal mode keeps the upstream piece strategy.

Rate/sequential/priority settings apply in the same engine. Changing peer capacity
re-adds **only the affected torrent** using resolved metadata and the pinned
storage sandbox, performs an on-disk check, and restores its prior pause intent.
It does not recreate the session, IPC owner or other torrents. Settings are
persisted after an engine acknowledgement, not after merely queueing a command.
The GUI receives the same confirmed shared model updates.

Seed stop conditions use independently observed real uploads in this process, never randomized
announce counters. Seed time counts active, completed time in this process;
pause time is excluded and the clock is not exported across restarts. Reaching
either condition pauses the torrent. This does not certify absence of leaks or
replace a long soak test.

## Server packages

Build packages CI produces separate `tui` Windows x64 ZIP and Linux x64 TAR.GZ
artifacts with only TUI/CLI/MCP binaries and documentation/examples. They are built
with `--no-default-features --features tui`; CI rejects GUI toolkit dependencies.
There are no desktop launcher/icon files in headless archives. Archive checksums,
strict allowlists and extracted binary smoke tests also cover this flavor.

Linux baseline is Ubuntu 24.04 / glibc 2.39, not Alpine/musl. No X11/Wayland is
required. Persistent private-tracker credentials still require a DBus session
and unlocked Secret Service keyring; no plaintext fallback is added for SSH.
Portable archives and separate unsigned native installers are available.
See [distribution](distribution.md) for isolated installation checks and limitations.

Trusted local storage/diagnostic commands include `:move-data ABSOLUTE_PARENT`
(exclusive complete same-volume payload only), `:remove-originals CONFIRM`
(permanent duplicate cleanup after a copy in this session), and
`:diagnostics ABSOLUTE_NEW_FILE` (redacted bounded report; no overwrite).
`:file-priority FILE_NUMBER high|normal|skip` uses one-based file numbers; a
`[high]` marker identifies high-priority files. File selection checkboxes remain
separate from high/normal ordering. See [runtime controls](runtime-controls.md)
for recheck, recovery and platform limits before using storage commands.
