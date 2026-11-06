# Disposable native UI candidate

The running user's Release GUI and background owner remain untouched. Two Cargo
examples compile the production entry points under different artifact names:
`ui-candidate` includes `src/main.rs`; `ui-candidate-daemon` delegates the normal
daemon entry point. They add no application test mode or permission bypass.

Build only when the shared build slot is free, using the existing target cache:

```powershell
cargo +stable build --locked --release --all-features --example ui-candidate --example ui-candidate-daemon --target x86_64-pc-windows-msvc
python scripts/prepare_ui_candidate.py --gui target/x86_64-pc-windows-msvc/release/examples/ui-candidate.exe --daemon target/x86_64-pc-windows-msvc/release/examples/ui-candidate-daemon.exe --output-parent ABSOLUTE_EVIDENCE_DIRECTORY
```

The packaging script requires the alias artifact names and executable headers,
copies them to a new GUID directory as `peerlume.exe`/`peerlume-daemon.exe`, and
records SHA-256 identities in `candidate.json`. It launches no process and refuses
ordinary installed executable names. It does not create the profile yet. The
manifest includes exact literal argument arrays for later launch and stopping
only that disposable owner.

Prepare the synthetic state through the actual `StateStore` APIs, not an invented
SQLite schema. Use the new profile path from `candidate.json`:

```powershell
$env:PEERLUME_UI_CANDIDATE_PROFILE = 'ABSOLUTE_NEW_CANDIDATE_DIRECTORY/profile'
cargo +stable test --locked --all-features --test ui_candidate_fixture -- --ignored --exact prepare_native_ui_candidate_profile --nocapture
```

This ignored fixture writer requires a new absolute directory and never launches
an engine. Its ordinary safety regression rejects existing/relative paths and
round-trips the real state/settings schema. The rich fixture contains 80 saved
paused torrents with long names; the first has 150 nested files. Private,
trackerless synthetic torrents and tiny local payloads need no external service.
DHT, PEX, LSD, UPnP, uTP and automatic blocklist updates are disabled; new torrents
and the saved fixture stay paused. The normal native owner still opens its local
control/listener sockets when explicitly launched. This is not proof of a bound
loopback transfer listener on Windows.

Downloaded and verified counters remain zero until real engine evidence arrives.
Do not label saved presentation data as observed Downloading/Error activity or
claim a transfer/repair test from these screenshots. Existing engine fixtures and
native lifecycle tests cover their own distinct behavior.

Only after preparation, launch the manifest's GUI command with its explicit
`--profile`. Inspect wide/narrow layouts, real scrollbar gutters, the hierarchy,
selection, dialogs, operation controls, and Light/Dark/System at 100/150/200%.
Close the frontend normally; the disposable owner is expected to remain alive.
Use the manifest's daemon `--stop` command or the explicit in-app confirmation
to stop that same owner. Never replace these commands with an unscoped stop/kill
or the user's normal executable/profile.

The Reliability X11 job now builds the sibling daemon along with the GUI/control
CLI, as production frontend startup requires it. The baseline c949 native-window
artifact showed the exact missing-daemon startup error. Changing that build
contract is not evidence that Linux window automation has since passed; inspect
the later run separately.
